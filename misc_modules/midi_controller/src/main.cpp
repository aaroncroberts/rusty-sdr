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
    /* Version:         */ 0, 2, 0,
    /* Max instances    */ 1
};

// ─────────────────────────────────────────────────────────────────────────────
// Config persistence (one JSON file shared across all instances, keyed by name)
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
//   Transport (CC, value 127=press / 0=release):
//     STOP=42  PLAY=41  REW=43  FF=44  REC=45  CYCLE=46
//   Track Prev/Next: CC 58, CC 59
//   S/M/R buttons (NoteOn): S=32–39, M=48–55, R=64–71
// ─────────────────────────────────────────────────────────────────────────────
namespace NK2Defaults {
    // Continuous controls
    constexpr int CC_TUNE_COARSE = 0;
    constexpr int CC_TUNE_FINE   = 1;
    constexpr int CC_GAIN        = 16;  // unsupported
    constexpr int CC_ZOOM        = 17;
    // Transport (CC buttons)
    constexpr int CC_PLAY        = 41;
    constexpr int CC_STOP        = 42;
    constexpr int CC_REW         = 43;
    constexpr int CC_FF          = 44;
    // Track buttons
    constexpr int CC_TRACK_PREV  = 58;
    constexpr int CC_TRACK_NEXT  = 59;
    // S/M/R buttons (Note numbers)
    constexpr int NOTE_S1        = 32;
    constexpr int NOTE_M1        = 48;
    constexpr int NOTE_R1        = 64;
    // Step sizes
    constexpr double STEP_COARSE_HZ = 1e6;    // 1 MHz
    constexpr double STEP_FINE_HZ   = 10e3;   // 10 kHz
    constexpr double STEP_MEDIUM_HZ = 100e3;  // 100 kHz (REW/FF buttons)
}

// ─────────────────────────────────────────────────────────────────────────────
// Actions
// ─────────────────────────────────────────────────────────────────────────────
enum class Action {
    // Continuous (slider/knob)
    TuneCoarse,
    TuneFine,
    Zoom,
    // Transport toggles
    Play,
    Stop,
    // Discrete step-tune (button press)
    StepTuneUp,
    StepTuneDown,
    // Band plan navigation (button press)
    BandPlanNext,
    BandPlanPrev,
    // Feature toggles (button press)
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

// Config keys matching the Action enum order
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
    int  cc      = -1;    // CC number, -1 = unassigned
    int  note    = -1;    // Note number, -1 = unassigned
    int  channel = -1;    // MIDI channel filter, -1 = any
    double stepHz = 0;    // for step-tune actions (Hz per step)
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

    // ── MIDI mappings ─────────────────────────────────────────────────────────
    ActionMap mappings[(int)Action::Count];

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
    // learnTarget = Action::Count → not learning
    Action learnTarget = Action::Count;

    // ── Action state ──────────────────────────────────────────────────────────
    bool muteActive = false;
    std::map<std::string, float> muteSavedVolumes;
    bool recorderArmed = false;
    std::string recorderInstanceName = "Recorder";

    // ── UI display state ──────────────────────────────────────────────────────
    std::string statusText    = "Not initialised";
    std::string lastEventText = "—";
    int         connectedCount = 0;

    // ─────────────────────────────────────────────────────────────────────────
    // Config load / save
    // ─────────────────────────────────────────────────────────────────────────
    void setDefaultMappings() {
        mappings[(int)Action::TuneCoarse]   = { NK2Defaults::CC_TUNE_COARSE, -1, -1, NK2Defaults::STEP_COARSE_HZ };
        mappings[(int)Action::TuneFine]     = { NK2Defaults::CC_TUNE_FINE,   -1, -1, NK2Defaults::STEP_FINE_HZ   };
        mappings[(int)Action::Zoom]         = { NK2Defaults::CC_ZOOM,        -1, -1, 0 };
        mappings[(int)Action::Play]         = { NK2Defaults::CC_PLAY,        -1, -1, 0 };
        mappings[(int)Action::Stop]         = { NK2Defaults::CC_STOP,        -1, -1, 0 };
        mappings[(int)Action::StepTuneUp]   = { NK2Defaults::CC_FF,          -1, -1, NK2Defaults::STEP_MEDIUM_HZ };
        mappings[(int)Action::StepTuneDown] = { NK2Defaults::CC_REW,         -1, -1, NK2Defaults::STEP_MEDIUM_HZ };
        mappings[(int)Action::BandPlanNext] = { NK2Defaults::CC_TRACK_NEXT,  -1, -1, 0 };
        mappings[(int)Action::BandPlanPrev] = { NK2Defaults::CC_TRACK_PREV,  -1, -1, 0 };
        mappings[(int)Action::VFOCycle]     = { -1, NK2Defaults::NOTE_S1,    -1, 0 };
        mappings[(int)Action::AudioMute]    = { -1, NK2Defaults::NOTE_M1,    -1, 0 };
        mappings[(int)Action::RecorderArm]  = { -1, NK2Defaults::NOTE_R1,    -1, 0 };
    }

    void loadConfig() {
        auto& cfg = config.conf[name];
        for (int i = 0; i < (int)Action::Count; i++) {
            const char* key = ACTION_CONFIG_KEYS[i];
            if (!cfg.contains(key)) continue;
            auto& m = cfg[key];
            mappings[i].cc      = m.value("cc",      -1);
            mappings[i].note    = m.value("note",    -1);
            mappings[i].channel = m.value("channel", -1);
            mappings[i].stepHz  = m.value("stepHz",  0.0);
        }
        if (cfg.contains("recorderInstance"))
            recorderInstanceName = cfg["recorderInstance"].get<std::string>();
    }

    void saveConfig() {
        auto& cfg = config.conf[name];
        for (int i = 0; i < (int)Action::Count; i++) {
            const char* key = ACTION_CONFIG_KEYS[i];
            cfg[key]["cc"]      = mappings[i].cc;
            cfg[key]["note"]    = mappings[i].note;
            cfg[key]["channel"] = mappings[i].channel;
            cfg[key]["stepHz"]  = mappings[i].stepHz;
        }
        cfg["recorderInstance"] = recorderInstanceName;
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
    // CoreMIDI read callback (runs on a private CoreMIDI thread)
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
            // MIDI learn: capture the first non-zero event and assign it
            if (learnTarget != Action::Count) {
                if (ev.type == MidiMsgType::CC && ev.value > 0) {
                    mappings[(int)learnTarget].cc   = ev.number;
                    mappings[(int)learnTarget].note = -1;
                    saveConfig();
                    learnTarget = Action::Count;
                    lastEventText = "Learned: CC " + std::to_string(ev.number);
                    continue;
                }
                if (ev.type == MidiMsgType::NoteOn && ev.value > 0) {
                    mappings[(int)learnTarget].note = ev.number;
                    mappings[(int)learnTarget].cc   = -1;
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
    // CC dispatch — looks up action by configured CC number
    // ─────────────────────────────────────────────────────────────────────────
    void handleCC(uint8_t cc, uint8_t value) {
        lastEventText = "CC " + std::to_string(cc) + " = " + std::to_string(value);

        // Continuous actions (use value directly, fire on any value)
        if (cc == (uint8_t)mappings[(int)Action::Zoom].cc) {
            double totalBW = sigpath::iqFrontEnd.getSampleRate();
            double t = value / 127.0;
            gui::waterfall.setViewBandwidth(1000.0 + (t * t * (totalBW - 1000.0)));
            return;
        }
        if (cc == (uint8_t)mappings[(int)Action::TuneCoarse].cc) {
            applyRelativeTune(cc, value, mappings[(int)Action::TuneCoarse].stepHz);
            return;
        }
        if (cc == (uint8_t)mappings[(int)Action::TuneFine].cc) {
            applyRelativeTune(cc, value, mappings[(int)Action::TuneFine].stepHz);
            return;
        }

        // Button actions — trigger on value > 0 (press), ignore 0 (release)
        if (value == 0) return;

        for (int i = 0; i < (int)Action::Count; i++) {
            if (mappings[i].cc == (int)cc) {
                fireAction((Action)i);
                return;
            }
        }
    }

    // Note-mapped button dispatch
    void handleNoteOn(uint8_t note) {
        lastEventText = "Note " + std::to_string(note);
        for (int i = 0; i < (int)Action::Count; i++) {
            if (mappings[i].note == (int)note) {
                fireAction((Action)i);
                return;
            }
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
            doStepTune(+1, mappings[(int)Action::StepTuneUp].stepHz);
            break;
        case Action::StepTuneDown:
            doStepTune(-1, mappings[(int)Action::StepTuneDown].stepHz);
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

    // Relative-delta tune from an absolute-position slider/knob.
    // Skips the first event (no previous value) and large jumps (wrap-around).
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
        double newFreq = std::max(0.0, current + delta * stepHz);
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfoName, newFreq);
    }

    // Discrete step tune: called on button press.
    void doStepTune(int direction, double stepHz) {
        if (stepHz <= 0) stepHz = 100e3;
        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfoName = !gui::waterfall.selectedVFO.empty()
            ? gui::waterfall.selectedVFO
            : gui::waterfall.vfos.begin()->first;
        double current = gui::waterfall.getCenterFrequency() + sigpath::vfoManager.getOffset(vfoName);
        double newFreq = std::max(0.0, current + direction * stepHz);
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfoName, newFreq);
    }

    // Step to the next or previous band plan entry.
    void doBandPlanStep(int direction) {
        if (!gui::waterfall.bandplan) return;
        auto& bands = gui::waterfall.bandplan->bands;
        if (bands.empty()) return;

        double centerFreq = gui::waterfall.getCenterFrequency();

        // Find band that contains current center frequency
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

    // Cycle through the available VFOs in the waterfall.
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

    // Toggle mute on all audio streams.
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

    // Toggle recording start/stop in the Recorder module.
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

        ImGui::Text("Status: %s", _this->statusText.c_str());
        ImGui::Text("Sources: %d connected", _this->connectedCount);
        ImGui::Text("Last: %s", _this->lastEventText.c_str());

        // State indicators
        if (_this->muteActive) {
            ImGui::SameLine();
            ImGui::TextColored(ImVec4(1, 0.3f, 0.3f, 1), "[MUTED]");
        }
        if (_this->recorderArmed) {
            ImGui::SameLine();
            ImGui::TextColored(ImVec4(1, 0.1f, 0.1f, 1), "[REC]");
        }

        ImGui::Separator();
        ImGui::TextUnformatted("MIDI Mapping");

        // Table: Action | Binding | Step | Learn
        if (ImGui::BeginTable("##midi_map", 4,
                ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit)) {
            ImGui::TableSetupColumn("Action",  ImGuiTableColumnFlags_WidthStretch);
            ImGui::TableSetupColumn("Binding", ImGuiTableColumnFlags_WidthFixed, 56.0f);
            ImGui::TableSetupColumn("Step",    ImGuiTableColumnFlags_WidthFixed, 72.0f);
            ImGui::TableSetupColumn("",        ImGuiTableColumnFlags_WidthFixed, 55.0f);
            ImGui::TableHeadersRow();

            for (int i = 0; i < (int)Action::Count; i++) {
                auto& m = _this->mappings[i];
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
                bool isLearning = (_this->learnTarget == (Action)i);
                char btnLabel[32];
                if (isLearning) {
                    ImGui::PushStyleColor(ImGuiCol_Button, ImVec4(0.8f, 0.2f, 0.2f, 1.0f));
                    snprintf(btnLabel, sizeof(btnLabel), "Cancel##lrn%d", i);
                    if (ImGui::SmallButton(btnLabel)) _this->learnTarget = Action::Count;
                    ImGui::PopStyleColor();
                } else {
                    snprintf(btnLabel, sizeof(btnLabel), "Learn##lrn%d", i);
                    if (ImGui::SmallButton(btnLabel)) _this->learnTarget = (Action)i;
                }
            }
            ImGui::EndTable();
        }

        if (_this->learnTarget != Action::Count) {
            ImGui::TextColored(ImVec4(1, 0.5f, 0, 1),
                "Move a CC or press a key to assign to '%s'",
                ACTION_NAMES[(int)_this->learnTarget]);
        }

        ImGui::Separator();
        if (ImGui::Button("Reset to nanoKontrol2 defaults")) {
            _this->setDefaultMappings();
            _this->saveConfig();
        }
        ImGui::SameLine();
        if (ImGui::Button("Reconnect MIDI")) {
            _this->shutdownMidi();
            _this->initMidi();
        }

        // Recorder instance name (configurable for multi-recorder setups)
        ImGui::Separator();
        ImGui::TextUnformatted("Recorder instance name:");
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
