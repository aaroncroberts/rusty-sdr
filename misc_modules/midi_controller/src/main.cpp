#include <imgui.h>
#include <module.h>
#include <gui/gui.h>
#include <gui/tuner.h>
#include <gui/main_window.h>
#include <gui/widgets/bandplan.h>
#include <signal_path/signal_path.h>
#include <signal_path/sink.h>
#include <core.h>
#include <config.h>
#include <utils/flog.h>

#include <recorder_interface.h>

#include <CoreMIDI/CoreMIDI.h>
#include <CoreFoundation/CoreFoundation.h>

#include <map>
#include <mutex>
#include <vector>
#include <string>
#include <algorithm>
#include <cmath>

SDRPP_MOD_INFO{
    /* Name:            */ "midi_controller",
    /* Description:     */ "MIDI controller integration (CoreMIDI) for SDR++ — tune, zoom, and transport via hardware knobs/sliders",
    /* Author:          */ "Aaron C. Roberts",
    /* Version:         */ 0, 3, 0,
    /* Max instances    */ 1
};

// ─────────────────────────────────────────────────────────────────────────────
// Config persistence
// ─────────────────────────────────────────────────────────────────────────────
ConfigManager config;

// ─────────────────────────────────────────────────────────────────────────────
// MIDI event
// ─────────────────────────────────────────────────────────────────────────────
enum class MidiMsgType { CC, NoteOn, NoteOff };

struct MidiEvent {
    MidiMsgType type;
    uint8_t channel;
    uint8_t number;
    uint8_t value;
};

// ─────────────────────────────────────────────────────────────────────────────
// Korg nanoKontrol2 Scene 1 factory CC/Note layout
//
//   Sliders (left→right): CC 0–7
//   Knobs   (left→right): CC 16–23
//   Transport (CC, 127=press / 0=release): STOP=42 PLAY=41 REW=43 FF=44 REC=45 CYCLE=46
//   Track Prev/Next: CC 58, CC 59
//   S/M/R buttons (NoteOn): S=32–39, M=48–55, R=64–71
// ─────────────────────────────────────────────────────────────────────────────
namespace NK2Defaults {
    constexpr int CC_TUNE_COARSE = 0;
    constexpr int CC_TUNE_FINE   = 1;
    constexpr int CC_ZOOM        = 17;
    constexpr int CC_PLAY        = 41;
    constexpr int CC_STOP        = 42;
    constexpr int CC_REW         = 43;
    constexpr int CC_FF          = 44;
    constexpr int CC_CYCLE       = 46;
    constexpr int CC_TRACK_PREV  = 58;
    constexpr int CC_TRACK_NEXT  = 59;
    constexpr int NOTE_S1        = 32;
    constexpr int NOTE_M1        = 48;
    constexpr int NOTE_R1        = 64;
    constexpr double STEP_COARSE_HZ = 1e6;
    constexpr double STEP_FINE_HZ   = 10e3;
    constexpr double STEP_MEDIUM_HZ = 100e3;
}

// ─────────────────────────────────────────────────────────────────────────────
// Pages
// ─────────────────────────────────────────────────────────────────────────────
static constexpr int PAGE_COUNT = 3;
static const char* PAGE_NAMES[PAGE_COUNT] = { "Tune", "Monitor", "Recorder" };

// ─────────────────────────────────────────────────────────────────────────────
// Actions (per-page bindable)
// ─────────────────────────────────────────────────────────────────────────────
enum class Action {
    TuneCoarse,
    TuneFine,
    Zoom,
    Play,
    Stop,
    StepTuneUp,
    StepTuneDown,
    BandPlanNext,
    BandPlanPrev,
    VFOCycle,
    AudioMute,
    RecorderArm,
    Count
};

static const char* ACTION_NAMES[] = {
    "Tune Coarse",
    "Tune Fine",
    "Zoom",
    "Play/Toggle",
    "Stop",
    "Step Tune Up",
    "Step Tune Down",
    "Band Plan Next",
    "Band Plan Prev",
    "VFO Cycle",
    "Mute Audio",
    "Rec Arm",
};

static const char* ACTION_CONFIG_KEYS[] = {
    "tuneCoarse",
    "tuneFine",
    "zoom",
    "play",
    "stop",
    "stepTuneUp",
    "stepTuneDown",
    "bandPlanNext",
    "bandPlanPrev",
    "vfoCycle",
    "audioMute",
    "recorderArm",
};

// ─────────────────────────────────────────────────────────────────────────────
// Per-action MIDI mapping
// ─────────────────────────────────────────────────────────────────────────────
struct ActionMap {
    int    cc      = -1;   // CC number, -1 = unassigned
    int    note    = -1;   // Note number, -1 = unassigned
    int    channel = -1;   // MIDI channel filter, -1 = any
    double stepHz  = 0;    // for step-tune actions
};

// ─────────────────────────────────────────────────────────────────────────────
// MidiControllerModule
// ─────────────────────────────────────────────────────────────────────────────
class MidiControllerModule : public ModuleManager::Instance {
public:
    MidiControllerModule(std::string name) : name(name) {
        if (!config.conf.contains(name)) {
            setDefaultMappings();
            saveConfig();
        } else {
            loadConfig();
        }
        gui::menu.registerEntry(name, menuHandler, this, NULL);
    }

    ~MidiControllerModule() {
        gui::menu.removeEntry(name);
        shutdownMidi();
    }

    void postInit() { initMidi(); }

    void enable() {
        enabled = true;
        if (midiClient == 0) initMidi();
    }

    void disable() {
        enabled = false;
        shutdownMidi();
    }

    bool isEnabled() { return enabled; }

private:
    // ── Module state ─────────────────────────────────────────────────────────
    std::string name;
    bool        enabled = true;

    // ── Multi-page mappings ───────────────────────────────────────────────────
    ActionMap mappings[PAGE_COUNT][(int)Action::Count];
    int       currentPage = 0;

    // ── Global bindings (work on all pages) ───────────────────────────────────
    int cycleCC = NK2Defaults::CC_CYCLE;   // CC number for page advance

    // ── CoreMIDI state ────────────────────────────────────────────────────────
    MIDIClientRef   midiClient = 0;
    MIDIPortRef     inputPort  = 0;
    std::vector<MIDIEndpointRef> connectedSources;

    // ── Event queue (CoreMIDI thread → render thread) ─────────────────────────
    std::mutex             eventMutex;
    std::vector<MidiEvent> eventQueue;

    // ── Previous CC values for relative delta detection ───────────────────────
    uint8_t prevCC[128]      = {};
    bool    prevCCKnown[128] = {};

    // ── MIDI learn state ─────────────────────────────────────────────────────
    // learnTarget action = Action::Count → not learning
    Action learnTarget     = Action::Count;
    bool   learningCycleCC = false;

    // ── Action state ──────────────────────────────────────────────────────────
    bool muteActive = false;
    std::map<std::string, float> muteSavedVolumes;
    bool recorderArmed = false;
    std::string recorderInstanceName = "Recorder";

    // ── UI display state ──────────────────────────────────────────────────────
    std::string statusText     = "Not initialised";
    std::string lastEventText  = "—";
    int         connectedCount = 0;

    // ─────────────────────────────────────────────────────────────────────────
    // Config load / save
    // ─────────────────────────────────────────────────────────────────────────
    void setDefaultMappings() {
        // All pages start empty
        for (int p = 0; p < PAGE_COUNT; p++) {
            for (int i = 0; i < (int)Action::Count; i++) {
                mappings[p][i] = {};
            }
        }

        // Page 0 (Tune) — full nanoKontrol2 defaults
        auto& t = mappings[0];
        t[(int)Action::TuneCoarse]   = { NK2Defaults::CC_TUNE_COARSE, -1, -1, NK2Defaults::STEP_COARSE_HZ };
        t[(int)Action::TuneFine]     = { NK2Defaults::CC_TUNE_FINE,   -1, -1, NK2Defaults::STEP_FINE_HZ   };
        t[(int)Action::Zoom]         = { NK2Defaults::CC_ZOOM,        -1, -1, 0 };
        t[(int)Action::Play]         = { NK2Defaults::CC_PLAY,        -1, -1, 0 };
        t[(int)Action::Stop]         = { NK2Defaults::CC_STOP,        -1, -1, 0 };
        t[(int)Action::StepTuneUp]   = { NK2Defaults::CC_FF,          -1, -1, NK2Defaults::STEP_MEDIUM_HZ };
        t[(int)Action::StepTuneDown] = { NK2Defaults::CC_REW,         -1, -1, NK2Defaults::STEP_MEDIUM_HZ };
        t[(int)Action::BandPlanNext] = { NK2Defaults::CC_TRACK_NEXT,  -1, -1, 0 };
        t[(int)Action::BandPlanPrev] = { NK2Defaults::CC_TRACK_PREV,  -1, -1, 0 };
        t[(int)Action::VFOCycle]     = { -1, NK2Defaults::NOTE_S1,    -1, 0 };
        t[(int)Action::AudioMute]    = { -1, NK2Defaults::NOTE_M1,    -1, 0 };
        t[(int)Action::RecorderArm]  = { -1, NK2Defaults::NOTE_R1,    -1, 0 };

        // Page 1 (Monitor) — keep tune + zoom + mute
        auto& m = mappings[1];
        m[(int)Action::TuneCoarse] = { NK2Defaults::CC_TUNE_COARSE, -1, -1, NK2Defaults::STEP_COARSE_HZ };
        m[(int)Action::TuneFine]   = { NK2Defaults::CC_TUNE_FINE,   -1, -1, NK2Defaults::STEP_FINE_HZ   };
        m[(int)Action::Zoom]       = { NK2Defaults::CC_ZOOM,        -1, -1, 0 };
        m[(int)Action::AudioMute]  = { -1, NK2Defaults::NOTE_M1,    -1, 0 };

        // Page 2 (Recorder) — tune, play, recorder arm
        auto& r = mappings[2];
        r[(int)Action::TuneCoarse]  = { NK2Defaults::CC_TUNE_COARSE, -1, -1, NK2Defaults::STEP_COARSE_HZ };
        r[(int)Action::TuneFine]    = { NK2Defaults::CC_TUNE_FINE,   -1, -1, NK2Defaults::STEP_FINE_HZ   };
        r[(int)Action::Play]        = { NK2Defaults::CC_PLAY,        -1, -1, 0 };
        r[(int)Action::Stop]        = { NK2Defaults::CC_STOP,        -1, -1, 0 };
        r[(int)Action::AudioMute]   = { -1, NK2Defaults::NOTE_M1,    -1, 0 };
        r[(int)Action::RecorderArm] = { -1, NK2Defaults::NOTE_R1,    -1, 0 };
    }

    void loadConfig() {
        auto& cfg = config.conf[name];

        // Load global bindings
        cycleCC              = cfg.value("cycleCC",           NK2Defaults::CC_CYCLE);
        recorderInstanceName = cfg.value("recorderInstance",  std::string("Recorder"));

        // Per-page mappings — stored under "pages" array
        if (cfg.contains("pages") && cfg["pages"].is_array()) {
            auto& pages = cfg["pages"];
            for (int p = 0; p < PAGE_COUNT && p < (int)pages.size(); p++) {
                auto& pg = pages[p];
                for (int i = 0; i < (int)Action::Count; i++) {
                    const char* key = ACTION_CONFIG_KEYS[i];
                    if (!pg.contains(key)) continue;
                    auto& m = pg[key];
                    mappings[p][i].cc      = m.value("cc",      -1);
                    mappings[p][i].note    = m.value("note",    -1);
                    mappings[p][i].channel = m.value("channel", -1);
                    mappings[p][i].stepHz  = m.value("stepHz",  0.0);
                }
            }
        } else {
            // Migrate legacy flat config (pre-v0.3) into page 0
            setDefaultMappings();
            for (int i = 0; i < (int)Action::Count; i++) {
                const char* key = ACTION_CONFIG_KEYS[i];
                if (!cfg.contains(key)) continue;
                auto& m = cfg[key];
                mappings[0][i].cc      = m.value("cc",      -1);
                mappings[0][i].note    = m.value("note",    -1);
                mappings[0][i].channel = m.value("channel", -1);
                mappings[0][i].stepHz  = m.value("stepHz",  0.0);
            }
        }
    }

    void saveConfig() {
        auto& cfg = config.conf[name];
        cfg["cycleCC"]          = cycleCC;
        cfg["recorderInstance"] = recorderInstanceName;

        cfg["pages"] = json::array();
        for (int p = 0; p < PAGE_COUNT; p++) {
            json pg = json::object();
            for (int i = 0; i < (int)Action::Count; i++) {
                const char* key = ACTION_CONFIG_KEYS[i];
                pg[key]["cc"]      = mappings[p][i].cc;
                pg[key]["note"]    = mappings[p][i].note;
                pg[key]["channel"] = mappings[p][i].channel;
                pg[key]["stepHz"]  = mappings[p][i].stepHz;
            }
            cfg["pages"].push_back(pg);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CoreMIDI initialisation
    // ─────────────────────────────────────────────────────────────────────────
    void initMidi() {
        if (midiClient != 0) return;

        CFStringRef clientName = CFStringCreateWithCString(
            kCFAllocatorDefault, "sdrpp_midi_controller", kCFStringEncodingUTF8);
        OSStatus st = MIDIClientCreate(clientName, nullptr, nullptr, &midiClient);
        CFRelease(clientName);

        if (st != noErr) {
            flog::error("MidiController: MIDIClientCreate failed ({})", (int)st);
            statusText = "MIDIClientCreate failed";
            return;
        }

        CFStringRef portName = CFStringCreateWithCString(
            kCFAllocatorDefault, "sdrpp_input", kCFStringEncodingUTF8);
        st = MIDIInputPortCreate(midiClient, portName, midiReadProc, this, &inputPort);
        CFRelease(portName);

        if (st != noErr) {
            flog::error("MidiController: MIDIInputPortCreate failed ({})", (int)st);
            statusText = "MIDIInputPortCreate failed";
            return;
        }

        connectAllSources();
    }

    void shutdownMidi() {
        for (auto src : connectedSources) MIDIPortDisconnectSource(inputPort, src);
        connectedSources.clear();
        connectedCount = 0;
        if (inputPort)  { MIDIPortDispose(inputPort);    inputPort  = 0; }
        if (midiClient) { MIDIClientDispose(midiClient); midiClient = 0; }
        statusText = "Disconnected";
    }

    void connectAllSources() {
        ItemCount n = MIDIGetNumberOfSources();
        if (n == 0) { statusText = "No MIDI sources found"; return; }

        bool foundNK2 = false;
        for (ItemCount i = 0; i < n; i++) {
            MIDIEndpointRef src = MIDIGetSource(i);
            std::string devName = endpointName(src);
            std::string lower = devName;
            std::transform(lower.begin(), lower.end(), lower.begin(), ::tolower);
            if (lower.find("nanokontrol") != std::string::npos) {
                connectSource(src, devName);
                foundNK2 = true;
            }
        }

        if (!foundNK2) {
            flog::warn("MidiController: nanoKONTROL2 not found — connecting all {} source(s)", (int)n);
            for (ItemCount i = 0; i < n; i++)
                connectSource(MIDIGetSource(i), endpointName(MIDIGetSource(i)));
        }

        connectedCount = (int)connectedSources.size();
        statusText = connectedCount > 0
            ? "Connected (" + std::to_string(connectedCount) + " source" + (connectedCount > 1 ? "s)" : ")")
            : "No matching sources";
    }

    void connectSource(MIDIEndpointRef src, const std::string& devName) {
        if (MIDIPortConnectSource(inputPort, src, nullptr) == noErr) {
            connectedSources.push_back(src);
            flog::info("MidiController: connected to '{}'", devName);
        } else {
            flog::error("MidiController: failed to connect '{}'", devName);
        }
    }

    static std::string endpointName(MIDIEndpointRef ep) {
        CFStringRef cfName = nullptr;
        if (MIDIObjectGetStringProperty(ep, kMIDIPropertyDisplayName, &cfName) == noErr && cfName) {
            char buf[256] = {};
            CFStringGetCString(cfName, buf, sizeof(buf), kCFStringEncodingUTF8);
            CFRelease(cfName);
            return buf;
        }
        return "<unnamed>";
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CoreMIDI read callback (CoreMIDI thread)
    // ─────────────────────────────────────────────────────────────────────────
    static void midiReadProc(const MIDIPacketList* pktList,
                             void* readProcRefCon, void*) {
        auto* self = reinterpret_cast<MidiControllerModule*>(readProcRefCon);
        const MIDIPacket* pkt = &pktList->packet[0];

        std::lock_guard<std::mutex> lk(self->eventMutex);
        for (UInt32 i = 0; i < pktList->numPackets; i++) {
            if (pkt->length >= 3) {
                uint8_t status  = pkt->data[0] & 0xF0;
                uint8_t channel = pkt->data[0] & 0x0F;
                MidiEvent ev{ MidiMsgType::CC, channel, pkt->data[1], pkt->data[2] };
                if      (status == 0xB0) ev.type = MidiMsgType::CC;
                else if (status == 0x90) ev.type = MidiMsgType::NoteOn;
                else if (status == 0x80) ev.type = MidiMsgType::NoteOff;
                else { pkt = MIDIPacketNext(pkt); continue; }
                self->eventQueue.push_back(ev);
            }
            pkt = MIDIPacketNext(pkt);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Dispatch events — called from ImGui render thread each frame
    // ─────────────────────────────────────────────────────────────────────────
    void dispatchEvents() {
        std::vector<MidiEvent> local;
        {
            std::lock_guard<std::mutex> lk(eventMutex);
            local.swap(eventQueue);
        }

        for (auto& ev : local) {
            // MIDI learn capture
            if (learningCycleCC) {
                if (ev.type == MidiMsgType::CC && ev.value > 0) {
                    cycleCC = ev.number;
                    saveConfig();
                    learningCycleCC = false;
                    lastEventText = "CYCLE learned: CC " + std::to_string(ev.number);
                    continue;
                }
            } else if (learnTarget != Action::Count) {
                if (ev.type == MidiMsgType::CC && ev.value > 0) {
                    mappings[currentPage][(int)learnTarget].cc   = ev.number;
                    mappings[currentPage][(int)learnTarget].note = -1;
                    saveConfig();
                    learnTarget = Action::Count;
                    lastEventText = "Learned: CC " + std::to_string(ev.number);
                    continue;
                }
                if (ev.type == MidiMsgType::NoteOn && ev.value > 0) {
                    mappings[currentPage][(int)learnTarget].note = ev.number;
                    mappings[currentPage][(int)learnTarget].cc   = -1;
                    saveConfig();
                    learnTarget = Action::Count;
                    lastEventText = "Learned: Note " + std::to_string(ev.number);
                    continue;
                }
            }

            if (ev.type == MidiMsgType::CC) {
                handleCC(ev.number, ev.value);
            } else if (ev.type == MidiMsgType::NoteOn && ev.value > 0) {
                handleNoteOn(ev.number);
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CC dispatch
    // ─────────────────────────────────────────────────────────────────────────
    void handleCC(uint8_t cc, uint8_t value) {
        lastEventText = "CC " + std::to_string(cc) + " = " + std::to_string(value);

        // Global: CYCLE advances the page
        if (cc == (uint8_t)cycleCC && value > 0) {
            currentPage = (currentPage + 1) % PAGE_COUNT;
            flog::info("MidiController: page → {} ({})", currentPage, PAGE_NAMES[currentPage]);
            return;
        }

        auto* pg = mappings[currentPage];

        // Continuous actions
        if (cc == (uint8_t)pg[(int)Action::Zoom].cc) {
            double totalBW = sigpath::iqFrontEnd.getSampleRate();
            double t = value / 127.0;
            gui::waterfall.setViewBandwidth(1000.0 + (t * t * (totalBW - 1000.0)));
            return;
        }
        if (cc == (uint8_t)pg[(int)Action::TuneCoarse].cc) {
            applyRelativeTune(cc, value, pg[(int)Action::TuneCoarse].stepHz);
            return;
        }
        if (cc == (uint8_t)pg[(int)Action::TuneFine].cc) {
            applyRelativeTune(cc, value, pg[(int)Action::TuneFine].stepHz);
            return;
        }

        // Button actions
        if (value == 0) return;
        for (int i = 0; i < (int)Action::Count; i++) {
            if (pg[i].cc == (int)cc) { fireAction((Action)i); return; }
        }
    }

    void handleNoteOn(uint8_t note) {
        lastEventText = "Note " + std::to_string(note);
        auto* pg = mappings[currentPage];
        for (int i = 0; i < (int)Action::Count; i++) {
            if (pg[i].note == (int)note) { fireAction((Action)i); return; }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Action implementations
    // ─────────────────────────────────────────────────────────────────────────
    void fireAction(Action a) {
        switch (a) {
        case Action::Play:
            gui::mainWindow.setPlayState(!gui::mainWindow.sdrIsRunning());
            break;
        case Action::Stop:
            if (gui::mainWindow.sdrIsRunning()) gui::mainWindow.setPlayState(false);
            break;
        case Action::StepTuneUp:
            doStepTune(+1, mappings[currentPage][(int)Action::StepTuneUp].stepHz);
            break;
        case Action::StepTuneDown:
            doStepTune(-1, mappings[currentPage][(int)Action::StepTuneDown].stepHz);
            break;
        case Action::BandPlanNext:
            doBandPlanStep(+1);
            break;
        case Action::BandPlanPrev:
            doBandPlanStep(-1);
            break;
        case Action::VFOCycle:
            doVFOCycle();
            break;
        case Action::AudioMute:
            doAudioMute();
            break;
        case Action::RecorderArm:
            doRecorderArm();
            break;
        default:
            break;
        }
    }

    void applyRelativeTune(uint8_t cc, uint8_t value, double stepHz) {
        uint8_t prev  = prevCC[cc];
        bool    known = prevCCKnown[cc];
        prevCC[cc]      = value;
        prevCCKnown[cc] = true;

        if (!known) return;
        int delta = (int)value - (int)prev;
        if (std::abs(delta) >= 64 || delta == 0) return;

        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfoName = !gui::waterfall.selectedVFO.empty()
            ? gui::waterfall.selectedVFO
            : gui::waterfall.vfos.begin()->first;
        double current = gui::waterfall.getCenterFrequency() + sigpath::vfoManager.getOffset(vfoName);
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfoName, std::max(0.0, current + delta * stepHz));
    }

    void doStepTune(int direction, double stepHz) {
        if (stepHz <= 0) stepHz = 100e3;
        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfoName = !gui::waterfall.selectedVFO.empty()
            ? gui::waterfall.selectedVFO
            : gui::waterfall.vfos.begin()->first;
        double current = gui::waterfall.getCenterFrequency() + sigpath::vfoManager.getOffset(vfoName);
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfoName, std::max(0.0, current + direction * stepHz));
    }

    void doBandPlanStep(int direction) {
        if (!gui::waterfall.bandplan) return;
        auto& bands = gui::waterfall.bandplan->bands;
        if (bands.empty()) return;

        double centerFreq = gui::waterfall.getCenterFrequency();
        int idx = -1;
        for (int i = 0; i < (int)bands.size(); i++) {
            if (centerFreq >= bands[i].start && centerFreq <= bands[i].end) {
                idx = i;
                break;
            }
        }

        int nextIdx;
        if (direction > 0)
            nextIdx = (idx < 0) ? 0 : std::min(idx + 1, (int)bands.size() - 1);
        else
            nextIdx = (idx < 0) ? (int)bands.size() - 1 : std::max(idx - 1, 0);

        if (nextIdx == idx) return;

        double targetFreq = (bands[nextIdx].start + bands[nextIdx].end) / 2.0;
        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfoName = !gui::waterfall.selectedVFO.empty()
            ? gui::waterfall.selectedVFO
            : gui::waterfall.vfos.begin()->first;
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfoName, targetFreq);
    }

    void doVFOCycle() {
        auto& vfos = gui::waterfall.vfos;
        if (vfos.size() <= 1) return;
        auto it = vfos.find(gui::waterfall.selectedVFO);
        if (it == vfos.end())
            it = vfos.begin();
        else {
            ++it;
            if (it == vfos.end()) it = vfos.begin();
        }
        gui::waterfall.selectedVFO = it->first;
        gui::waterfall.selectedVFOChanged = true;
    }

    void doAudioMute() {
        auto names = sigpath::sinkManager.getStreamNames();
        if (names.empty()) return;
        if (!muteActive) {
            muteSavedVolumes.clear();
            for (auto& n : names) {
                muteSavedVolumes[n] = sigpath::sinkManager.getStreamVolume(n);
                sigpath::sinkManager.setStreamVolume(n, 0.0f);
            }
            muteActive = true;
        } else {
            for (auto& n : names) {
                float vol = muteSavedVolumes.count(n) ? muteSavedVolumes[n] : 1.0f;
                sigpath::sinkManager.setStreamVolume(n, vol);
            }
            muteActive = false;
        }
    }

    void doRecorderArm() {
        if (recorderArmed) {
            core::modComManager.callInterface(recorderInstanceName, RECORDER_IFACE_CMD_STOP, NULL, NULL);
            recorderArmed = false;
        } else {
            core::modComManager.callInterface(recorderInstanceName, RECORDER_IFACE_CMD_START, NULL, NULL);
            recorderArmed = true;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ImGui side-panel menu
    // ─────────────────────────────────────────────────────────────────────────
    static void menuHandler(void* ctx) {
        auto* _this = reinterpret_cast<MidiControllerModule*>(ctx);
        _this->dispatchEvents();

        // ── Status row ───────────────────────────────────────────────────────
        ImGui::Text("Status: %s", _this->statusText.c_str());
        ImGui::Text("Sources: %d connected", _this->connectedCount);
        ImGui::Text("Last: %s", _this->lastEventText.c_str());
        if (_this->muteActive) {
            ImGui::SameLine();
            ImGui::TextColored(ImVec4(1, 0.3f, 0.3f, 1), "[MUTED]");
        }
        if (_this->recorderArmed) {
            ImGui::SameLine();
            ImGui::TextColored(ImVec4(1, 0.1f, 0.1f, 1), "[REC]");
        }

        // ── Page indicator ───────────────────────────────────────────────────
        ImGui::Separator();
        ImGui::Text("Page: ");
        for (int p = 0; p < PAGE_COUNT; p++) {
            ImGui::SameLine();
            bool isActive = (p == _this->currentPage);
            if (isActive)
                ImGui::TextColored(ImVec4(0.2f, 1.0f, 0.4f, 1), "[%s]", PAGE_NAMES[p]);
            else
                ImGui::TextDisabled("%s", PAGE_NAMES[p]);
        }
        {
            char cycleBuf[24];
            if (_this->cycleCC >= 0) snprintf(cycleBuf, sizeof(cycleBuf), "CC %d", _this->cycleCC);
            else                      snprintf(cycleBuf, sizeof(cycleBuf), "—");
            ImGui::Text("CYCLE: %s", cycleBuf);
            ImGui::SameLine();
            if (_this->learningCycleCC) {
                ImGui::PushStyleColor(ImGuiCol_Button, ImVec4(0.8f, 0.2f, 0.2f, 1.0f));
                if (ImGui::SmallButton("Cancel##cyc")) _this->learningCycleCC = false;
                ImGui::PopStyleColor();
            } else {
                if (ImGui::SmallButton("Learn##cyc")) {
                    _this->learningCycleCC = true;
                    _this->learnTarget = Action::Count;
                }
            }
        }
        if (_this->learningCycleCC)
            ImGui::TextColored(ImVec4(1, 0.5f, 0, 1), "Move a CC to assign CYCLE...");

        // ── Mapping table for current page ───────────────────────────────────
        ImGui::Separator();
        ImGui::Text("Mappings — %s page", PAGE_NAMES[_this->currentPage]);

        if (ImGui::BeginTable("##midi_map", 4,
                ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit)) {
            ImGui::TableSetupColumn("Action",  ImGuiTableColumnFlags_WidthStretch);
            ImGui::TableSetupColumn("Binding", ImGuiTableColumnFlags_WidthFixed, 56.0f);
            ImGui::TableSetupColumn("Step",    ImGuiTableColumnFlags_WidthFixed, 72.0f);
            ImGui::TableSetupColumn("",        ImGuiTableColumnFlags_WidthFixed, 55.0f);
            ImGui::TableHeadersRow();

            for (int i = 0; i < (int)Action::Count; i++) {
                auto& m = _this->mappings[_this->currentPage][i];
                ImGui::TableNextRow();

                ImGui::TableSetColumnIndex(0);
                ImGui::TextUnformatted(ACTION_NAMES[i]);

                ImGui::TableSetColumnIndex(1);
                char bindBuf[16];
                if (m.cc >= 0)        snprintf(bindBuf, sizeof(bindBuf), "CC %d",  m.cc);
                else if (m.note >= 0) snprintf(bindBuf, sizeof(bindBuf), "N %d",   m.note);
                else                  snprintf(bindBuf, sizeof(bindBuf), "—");
                ImGui::TextUnformatted(bindBuf);

                ImGui::TableSetColumnIndex(2);
                if (m.stepHz > 0) {
                    char stepBuf[32];
                    if      (m.stepHz >= 1e6) snprintf(stepBuf, sizeof(stepBuf), "%.0f MHz", m.stepHz / 1e6);
                    else if (m.stepHz >= 1e3) snprintf(stepBuf, sizeof(stepBuf), "%.0f kHz", m.stepHz / 1e3);
                    else                      snprintf(stepBuf, sizeof(stepBuf), "%.0f Hz",  m.stepHz);
                    ImGui::TextUnformatted(stepBuf);
                } else {
                    ImGui::TextDisabled("—");
                }

                ImGui::TableSetColumnIndex(3);
                bool isLearning = (_this->learnTarget == (Action)i && !_this->learningCycleCC);
                char btnLabel[32];
                if (isLearning) {
                    ImGui::PushStyleColor(ImGuiCol_Button, ImVec4(0.8f, 0.2f, 0.2f, 1.0f));
                    snprintf(btnLabel, sizeof(btnLabel), "Cancel##lrn%d", i);
                    if (ImGui::SmallButton(btnLabel)) _this->learnTarget = Action::Count;
                    ImGui::PopStyleColor();
                } else {
                    snprintf(btnLabel, sizeof(btnLabel), "Learn##lrn%d", i);
                    if (ImGui::SmallButton(btnLabel)) {
                        _this->learnTarget = (Action)i;
                        _this->learningCycleCC = false;
                    }
                }
            }
            ImGui::EndTable();
        }

        if (_this->learnTarget != Action::Count && !_this->learningCycleCC) {
            ImGui::TextColored(ImVec4(1, 0.5f, 0, 1),
                "Move a CC or press a key to assign to '%s' (%s page)",
                ACTION_NAMES[(int)_this->learnTarget], PAGE_NAMES[_this->currentPage]);
        }

        // ── Controls ─────────────────────────────────────────────────────────
        ImGui::Separator();
        if (ImGui::Button("Reset page to NK2 defaults")) {
            _this->setDefaultMappings();
            _this->saveConfig();
        }
        ImGui::SameLine();
        if (ImGui::Button("Reconnect MIDI")) {
            _this->shutdownMidi();
            _this->initMidi();
        }

        // Recorder instance name
        ImGui::Separator();
        ImGui::TextUnformatted("Recorder module name:");
        char recBuf[64];
        snprintf(recBuf, sizeof(recBuf), "%s", _this->recorderInstanceName.c_str());
        ImGui::SetNextItemWidth(120.0f);
        if (ImGui::InputText("##rec_name", recBuf, sizeof(recBuf))) {
            _this->recorderInstanceName = recBuf;
            _this->saveConfig();
        }
    }
};

MOD_EXPORT void _INIT_() {
    config.setPath(core::args["root"].s() + "/midi_controller_config.json");
    config.load(json::object());
    config.enableAutoSave();
}

MOD_EXPORT ModuleManager::Instance* _CREATE_INSTANCE_(std::string name) {
    return new MidiControllerModule(name);
}

MOD_EXPORT void _DELETE_INSTANCE_(void* instance) {
    delete (MidiControllerModule*)instance;
}

MOD_EXPORT void _END_() {
    config.disableAutoSave();
    config.save();
}
