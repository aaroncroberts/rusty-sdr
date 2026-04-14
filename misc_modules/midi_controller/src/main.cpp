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
    /* Description:     */ "MIDI controller integration (CoreMIDI) for SDR++ — full nanoKontrol2 surface: 8-column tuning, hold-repeat transport, and step control",
    /* Author:          */ "Aaron C. Roberts",
    /* Version:         */ 0, 4, 0,
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
    uint8_t     channel;
    uint8_t     number;
    uint8_t     value;
};

// ─────────────────────────────────────────────────────────────────────────────
// Korg nanoKontrol2 factory CC/Note layout
//
//   Sliders (absolute 0-127): CC 0–7
//   Knobs   (absolute 0-127): CC 16–23
//   S buttons (NoteOn):       Notes 32–39
//   M buttons (NoteOn):       Notes 48–55
//   R buttons (NoteOn):       Notes 64–71
//   Transport (CC 127=press / 0=release):
//     PLAY=41 STOP=42 REW=43 FF=44 REC=45 CYCLE=46
//   Track nav (CC): PREV=58 NEXT=59
// ─────────────────────────────────────────────────────────────────────────────
namespace NK2 {
    constexpr int CC_SLIDER_BASE  = 0;   // CC0–CC7
    constexpr int CC_KNOB_BASE    = 16;  // CC16–CC23
    constexpr int NOTE_S_BASE     = 32;  // Notes 32–39
    constexpr int NOTE_M_BASE     = 48;  // Notes 48–55
    constexpr int NOTE_R_BASE     = 64;  // Notes 64–71

    constexpr int CC_PLAY         = 41;
    constexpr int CC_STOP         = 42;
    constexpr int CC_REW          = 43;
    constexpr int CC_FF           = 44;
    constexpr int CC_REC          = 45;
    constexpr int CC_CYCLE        = 46;
    constexpr int CC_TRACK_PREV   = 58;
    constexpr int CC_TRACK_NEXT   = 59;

    // Hold-repeat timing (seconds)
    constexpr double HOLD_INITIAL = 0.45;   // delay before first repeat
    constexpr double HOLD_FAST    = 0.12;   // normal repeat rate
    constexpr double HOLD_TURBO   = 0.06;   // after 2 s held, turbo rate

    // Default step for transport REW/FF
    constexpr double STEP_TRANSPORT_HZ = 100e3;
}

// ─────────────────────────────────────────────────────────────────────────────
// Pages
// ─────────────────────────────────────────────────────────────────────────────
static constexpr int PAGE_COUNT = 3;
static const char* PAGE_NAMES[PAGE_COUNT] = { "Tune", "Monitor", "Recorder" };

// ─────────────────────────────────────────────────────────────────────────────
// Column knob function
// ─────────────────────────────────────────────────────────────────────────────
enum class KnobFunc : int { None = 0, Zoom = 1, Volume = 2 };

static const char* KNOB_FUNC_NAMES[] = { "—", "Zoom", "Vol" };

// ─────────────────────────────────────────────────────────────────────────────
// Per-column MIDI binding
//
// Each of the 8 nanoKontrol2 columns has: slider + knob + S/M/R buttons.
// When any step size > 0, S/M/R buttons set the step mode for that column:
//   S = large step, M = medium step (default), R = small step.
// When all step sizes are 0, the column notes fall through to the transport
// action table — useful for column 8 as a function-button bank.
// ─────────────────────────────────────────────────────────────────────────────
struct ColumnMap {
    int      sliderCC     = -1;
    int      knobCC       = -1;
    int      sNote        = -1;
    int      mNote        = -1;
    int      rNote        = -1;
    double   stepLargeHz  = 0;    // S-button mode
    double   stepMediumHz = 0;    // M-button mode (default)
    double   stepSmallHz  = 0;    // R-button mode
    KnobFunc knobFunc     = KnobFunc::None;
};

// ─────────────────────────────────────────────────────────────────────────────
// Transport / global actions
// ─────────────────────────────────────────────────────────────────────────────
enum class Action {
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
    "play", "stop",
    "stepTuneUp", "stepTuneDown",
    "bandPlanNext", "bandPlanPrev",
    "vfoCycle", "audioMute", "recorderArm",
};

struct ActionMap {
    int    cc         = -1;
    int    note       = -1;
    int    channel    = -1;
    double stepHz     = 0;
    bool   holdRepeat = false;  // fire repeatedly while CC is held
};

// ─────────────────────────────────────────────────────────────────────────────
// Hold-repeat state (one entry per pressed CC button)
// ─────────────────────────────────────────────────────────────────────────────
struct HeldButton {
    uint8_t cc;
    Action  action;
    double  pressTime;
    double  nextFireTime;
};

// ─────────────────────────────────────────────────────────────────────────────
// MidiControllerModule
// ─────────────────────────────────────────────────────────────────────────────
class MidiControllerModule : public ModuleManager::Instance {
public:
    MidiControllerModule(std::string name) : name(name) {
        if (!config.conf.contains(name)) {
            resetToDefaults();
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

    void enable()    { enabled = true;  if (midiClient == 0) initMidi(); }
    void disable()   { enabled = false; shutdownMidi(); }
    bool isEnabled() { return enabled; }

private:
    // ── Module state ─────────────────────────────────────────────────────────
    std::string name;
    bool        enabled = true;

    // ── Column system: 8 columns × PAGE_COUNT pages ───────────────────────────
    ColumnMap columns[PAGE_COUNT][8];
    int       colStepMode[8] = { 1, 1, 1, 1, 1, 1, 1, 1 }; // 0=small 1=med 2=large

    // ── Transport action system ───────────────────────────────────────────────
    ActionMap mappings[PAGE_COUNT][(int)Action::Count];
    int       currentPage = 0;
    int       cycleCC     = NK2::CC_CYCLE;

    // ── Hold-repeat ───────────────────────────────────────────────────────────
    std::vector<HeldButton> heldButtons;

    // ── CoreMIDI ──────────────────────────────────────────────────────────────
    MIDIClientRef midiClient = 0;
    MIDIPortRef   inputPort  = 0;
    std::vector<MIDIEndpointRef> connectedSources;

    // ── Event queue (CoreMIDI thread → render thread) ─────────────────────────
    std::mutex             eventMutex;
    std::vector<MidiEvent> eventQueue;

    // ── Delta tracking for sliders ────────────────────────────────────────────
    uint8_t prevCC[128]      = {};
    bool    prevCCKnown[128] = {};

    // ── MIDI learn state ──────────────────────────────────────────────────────
    Action learnActionTarget = Action::Count;
    bool   learningCycleCC   = false;

    // ── Runtime state ─────────────────────────────────────────────────────────
    bool muteActive = false;
    std::map<std::string, float> muteSavedVolumes;
    bool recorderArmed = false;
    std::string recorderInstanceName = "Recorder";

    // ── UI display ────────────────────────────────────────────────────────────
    std::string statusText    = "Not initialised";
    std::string lastEventText = "—";
    int         connectedCount = 0;

    // ─────────────────────────────────────────────────────────────────────────
    // Default mappings
    // ─────────────────────────────────────────────────────────────────────────
    void resetToDefaults() {
        for (int p = 0; p < PAGE_COUNT; p++)
            for (int c = 0; c < 8; c++)
                columns[p][c] = {};
        for (int p = 0; p < PAGE_COUNT; p++)
            for (int i = 0; i < (int)Action::Count; i++)
                mappings[p][i] = {};

        // ── Column step ladder (Pages 0 & 1) ─────────────────────────────────
        //
        //  Col 1: ±100 MHz / 10 MHz / 1 MHz        Knob → Zoom
        //  Col 2: ±10 MHz  / 1 MHz  / 100 kHz      Knob → Zoom
        //  Col 3: ±1 MHz   / 100 kHz/ 10 kHz       Knob → —
        //  Col 4: ±100 kHz / 10 kHz / 1 kHz        Knob → —
        //  Col 5: ±10 kHz  / 1 kHz  / 100 Hz       Knob → —
        //  Col 6: ±1 kHz   / 100 Hz / 10 Hz        Knob → Volume
        //  Col 7: ±100 Hz  / 10 Hz  / 1 Hz         Knob → Volume
        //  Col 8: slider unused; S8=VFO M8=Mute R8=RecArm  Knob → —
        //
        //  Press S on any column → large step active (shown in UI)
        //  Press M on any column → medium step active (default)
        //  Press R on any column → small step active
        static const double BIG[7]   = { 100e6, 10e6, 1e6, 100e3, 10e3, 1e3, 100 };
        static const double MED[7]   = {  10e6,  1e6, 100e3, 10e3, 1e3, 100,  10 };
        static const double SMALL[7] = {   1e6, 100e3, 10e3,  1e3, 100,  10,   1 };

        for (int p = 0; p < 2; p++) {
            for (int c = 0; c < 7; c++) {
                auto& col        = columns[p][c];
                col.sliderCC     = NK2::CC_SLIDER_BASE + c;
                col.knobCC       = NK2::CC_KNOB_BASE   + c;
                col.sNote        = NK2::NOTE_S_BASE    + c;
                col.mNote        = NK2::NOTE_M_BASE    + c;
                col.rNote        = NK2::NOTE_R_BASE    + c;
                col.stepLargeHz  = BIG[c];
                col.stepMediumHz = MED[c];
                col.stepSmallHz  = SMALL[c];
            }
            columns[p][0].knobFunc = KnobFunc::Zoom;
            columns[p][1].knobFunc = KnobFunc::Zoom;
            columns[p][5].knobFunc = KnobFunc::Volume;
            columns[p][6].knobFunc = KnobFunc::Volume;

            // Col 8 (index 7): function-button bank — no slider steps
            auto& c7   = columns[p][7];
            c7.knobCC  = NK2::CC_KNOB_BASE + 7;  // CC23 — spare
            c7.sNote   = NK2::NOTE_S_BASE  + 7;  // Note 39 → VFO cycle
            c7.mNote   = NK2::NOTE_M_BASE  + 7;  // Note 55 → mute
            c7.rNote   = NK2::NOTE_R_BASE  + 7;  // Note 71 → rec arm
        }

        // ── Page 0 transport (Tune) ───────────────────────────────────────────
        {
            auto* pg = mappings[0];
            pg[(int)Action::Play]        = { NK2::CC_PLAY,       -1,                    -1, 0,                     false };
            pg[(int)Action::Stop]        = { NK2::CC_STOP,       -1,                    -1, 0,                     false };
            pg[(int)Action::StepTuneDown]= { NK2::CC_REW,        -1,                    -1, NK2::STEP_TRANSPORT_HZ, true  };
            pg[(int)Action::StepTuneUp]  = { NK2::CC_FF,         -1,                    -1, NK2::STEP_TRANSPORT_HZ, true  };
            pg[(int)Action::RecorderArm] = { NK2::CC_REC,        NK2::NOTE_R_BASE + 7,  -1, 0,                     false };
            pg[(int)Action::BandPlanPrev]= { NK2::CC_TRACK_PREV, -1,                    -1, 0,                     true  };
            pg[(int)Action::BandPlanNext]= { NK2::CC_TRACK_NEXT, -1,                    -1, 0,                     true  };
            pg[(int)Action::VFOCycle]    = { -1, NK2::NOTE_S_BASE + 7,                  -1, 0,                     false };
            pg[(int)Action::AudioMute]   = { -1, NK2::NOTE_M_BASE + 7,                  -1, 0,                     false };
        }

        // ── Page 1 transport (Monitor) ────────────────────────────────────────
        {
            auto* pg = mappings[1];
            pg[(int)Action::StepTuneDown]= { NK2::CC_REW,        -1,                    -1, NK2::STEP_TRANSPORT_HZ, true  };
            pg[(int)Action::StepTuneUp]  = { NK2::CC_FF,         -1,                    -1, NK2::STEP_TRANSPORT_HZ, true  };
            pg[(int)Action::BandPlanPrev]= { NK2::CC_TRACK_PREV, -1,                    -1, 0,                     true  };
            pg[(int)Action::BandPlanNext]= { NK2::CC_TRACK_NEXT, -1,                    -1, 0,                     true  };
            pg[(int)Action::VFOCycle]    = { -1, NK2::NOTE_S_BASE + 7,                  -1, 0,                     false };
            pg[(int)Action::AudioMute]   = { -1, NK2::NOTE_M_BASE + 7,                  -1, 0,                     false };
        }

        // ── Page 2 (Recorder): 2-column tuning + record controls ─────────────
        {
            for (int c = 0; c < 2; c++) {
                auto& col        = columns[2][c];
                col.sliderCC     = NK2::CC_SLIDER_BASE + c;
                col.knobCC       = NK2::CC_KNOB_BASE   + c;
                col.sNote        = NK2::NOTE_S_BASE    + c;
                col.mNote        = NK2::NOTE_M_BASE    + c;
                col.rNote        = NK2::NOTE_R_BASE    + c;
                col.stepLargeHz  = (c == 0) ? 1e6   : 100e3;
                col.stepMediumHz = (c == 0) ? 100e3 : 10e3;
                col.stepSmallHz  = (c == 0) ? 10e3  : 1e3;
            }
            columns[2][0].knobFunc = KnobFunc::Zoom;
            columns[2][1].knobFunc = KnobFunc::Volume;

            auto* pg = mappings[2];
            pg[(int)Action::Play]       = { NK2::CC_PLAY, -1,                  -1, 0, false };
            pg[(int)Action::Stop]       = { NK2::CC_STOP, -1,                  -1, 0, false };
            pg[(int)Action::RecorderArm]= { NK2::CC_REC,  NK2::NOTE_R_BASE,    -1, 0, false };
            pg[(int)Action::AudioMute]  = { -1,           NK2::NOTE_M_BASE,    -1, 0, false };
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Config load / save
    // ─────────────────────────────────────────────────────────────────────────
    void loadConfig() {
        auto& cfg = config.conf[name];

        cycleCC              = cfg.value("cycleCC",          NK2::CC_CYCLE);
        recorderInstanceName = cfg.value("recorderInstance", std::string("Recorder"));

        // Columns
        if (cfg.contains("columns") && cfg["columns"].is_array()) {
            auto& pagesJson = cfg["columns"];
            for (int p = 0; p < PAGE_COUNT && p < (int)pagesJson.size(); p++) {
                auto& colsJson = pagesJson[p];
                if (!colsJson.is_array()) continue;
                for (int c = 0; c < 8 && c < (int)colsJson.size(); c++) {
                    auto& j      = colsJson[c];
                    auto& col    = columns[p][c];
                    col.sliderCC     = j.value("sliderCC",     -1);
                    col.knobCC       = j.value("knobCC",       -1);
                    col.sNote        = j.value("sNote",        -1);
                    col.mNote        = j.value("mNote",        -1);
                    col.rNote        = j.value("rNote",        -1);
                    col.stepLargeHz  = j.value("stepLargeHz",  0.0);
                    col.stepMediumHz = j.value("stepMediumHz", 0.0);
                    col.stepSmallHz  = j.value("stepSmallHz",  0.0);
                    col.knobFunc     = (KnobFunc)j.value("knobFunc", 0);
                }
            }
        }

        // Transport actions
        if (cfg.contains("pages") && cfg["pages"].is_array()) {
            auto& pages = cfg["pages"];
            for (int p = 0; p < PAGE_COUNT && p < (int)pages.size(); p++) {
                auto& pg = pages[p];
                for (int i = 0; i < (int)Action::Count; i++) {
                    const char* key = ACTION_CONFIG_KEYS[i];
                    if (!pg.contains(key)) continue;
                    auto& m               = pg[key];
                    mappings[p][i].cc         = m.value("cc",         -1);
                    mappings[p][i].note       = m.value("note",       -1);
                    mappings[p][i].channel    = m.value("channel",    -1);
                    mappings[p][i].stepHz     = m.value("stepHz",     0.0);
                    mappings[p][i].holdRepeat = m.value("holdRepeat", false);
                }
            }
        } else {
            // Legacy flat config → migrate
            resetToDefaults();
        }
    }

    void saveConfig() {
        auto& cfg = config.conf[name];
        cfg["cycleCC"]          = cycleCC;
        cfg["recorderInstance"] = recorderInstanceName;

        cfg["columns"] = json::array();
        for (int p = 0; p < PAGE_COUNT; p++) {
            json colsJson = json::array();
            for (int c = 0; c < 8; c++) {
                auto& col = columns[p][c];
                colsJson.push_back({
                    {"sliderCC",     col.sliderCC},
                    {"knobCC",       col.knobCC},
                    {"sNote",        col.sNote},
                    {"mNote",        col.mNote},
                    {"rNote",        col.rNote},
                    {"stepLargeHz",  col.stepLargeHz},
                    {"stepMediumHz", col.stepMediumHz},
                    {"stepSmallHz",  col.stepSmallHz},
                    {"knobFunc",     (int)col.knobFunc},
                });
            }
            cfg["columns"].push_back(colsJson);
        }

        cfg["pages"] = json::array();
        for (int p = 0; p < PAGE_COUNT; p++) {
            json pg = json::object();
            for (int i = 0; i < (int)Action::Count; i++) {
                const char* key = ACTION_CONFIG_KEYS[i];
                pg[key] = {
                    {"cc",         mappings[p][i].cc},
                    {"note",       mappings[p][i].note},
                    {"channel",    mappings[p][i].channel},
                    {"stepHz",     mappings[p][i].stepHz},
                    {"holdRepeat", mappings[p][i].holdRepeat},
                };
            }
            cfg["pages"].push_back(pg);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CoreMIDI
    // ─────────────────────────────────────────────────────────────────────────
    void initMidi() {
        if (midiClient != 0) return;

        CFStringRef cname = CFStringCreateWithCString(kCFAllocatorDefault,
                                "sdrpp_midi_controller", kCFStringEncodingUTF8);
        OSStatus st = MIDIClientCreate(cname, nullptr, nullptr, &midiClient);
        CFRelease(cname);
        if (st != noErr) { statusText = "MIDIClientCreate failed"; return; }

        CFStringRef pname = CFStringCreateWithCString(kCFAllocatorDefault,
                                "sdrpp_input", kCFStringEncodingUTF8);
        st = MIDIInputPortCreate(midiClient, pname, midiReadProc, this, &inputPort);
        CFRelease(pname);
        if (st != noErr) { statusText = "MIDIInputPortCreate failed"; return; }

        connectAllSources();
    }

    void shutdownMidi() {
        for (auto src : connectedSources) MIDIPortDisconnectSource(inputPort, src);
        connectedSources.clear();
        connectedCount = 0;
        heldButtons.clear();
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
            std::string lower   = devName;
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
        statusText     = connectedCount > 0
            ? "Connected (" + std::to_string(connectedCount) + " source" + (connectedCount > 1 ? "s)" : ")")
            : "No matching sources";
    }

    void connectSource(MIDIEndpointRef src, const std::string& devName) {
        if (MIDIPortConnectSource(inputPort, src, nullptr) == noErr) {
            connectedSources.push_back(src);
            flog::info("MidiController: connected to '{}'", devName);
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
    // Dispatch — called from render thread every frame via menuHandler
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
                    lastEventText = "CYCLE → CC " + std::to_string(ev.number);
                    continue;
                }
            } else if (learnActionTarget != Action::Count) {
                if (ev.type == MidiMsgType::CC && ev.value > 0) {
                    mappings[currentPage][(int)learnActionTarget].cc   = ev.number;
                    mappings[currentPage][(int)learnActionTarget].note = -1;
                    saveConfig();
                    learnActionTarget = Action::Count;
                    lastEventText = "Learned → CC " + std::to_string(ev.number);
                    continue;
                }
                if (ev.type == MidiMsgType::NoteOn && ev.value > 0) {
                    mappings[currentPage][(int)learnActionTarget].note = ev.number;
                    mappings[currentPage][(int)learnActionTarget].cc   = -1;
                    saveConfig();
                    learnActionTarget = Action::Count;
                    lastEventText = "Learned → Note " + std::to_string(ev.number);
                    continue;
                }
            }

            if      (ev.type == MidiMsgType::CC)                      handleCC(ev.number, ev.value);
            else if (ev.type == MidiMsgType::NoteOn  && ev.value > 0) handleNoteOn(ev.number);
        }

        // ── Hold-repeat tick ──────────────────────────────────────────────────
        double now = ImGui::GetTime();
        for (auto& h : heldButtons) {
            if (now < h.nextFireTime) continue;
            fireAction(h.action);
            double elapsed    = now - h.pressTime;
            double repeatRate = (elapsed > 2.0) ? NK2::HOLD_TURBO : NK2::HOLD_FAST;
            h.nextFireTime    = now + repeatRate;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CC handler
    // ─────────────────────────────────────────────────────────────────────────
    void handleCC(uint8_t cc, uint8_t value) {
        lastEventText = "CC " + std::to_string(cc) + " = " + std::to_string(value);

        // Global: CYCLE advances page
        if (cc == (uint8_t)cycleCC) {
            if (value > 0) currentPage = (currentPage + 1) % PAGE_COUNT;
            return;
        }

        // Column sliders and knobs
        for (int c = 0; c < 8; c++) {
            const auto& col = columns[currentPage][c];
            if (col.sliderCC == (int)cc) { applyColumnSlider(c, col, value); return; }
            if (col.knobCC   == (int)cc) { applyColumnKnob(col, value);      return; }
        }

        // Transport action CC buttons
        auto* pg = mappings[currentPage];
        for (int i = 0; i < (int)Action::Count; i++) {
            if (pg[i].cc != (int)cc) continue;
            if (pg[i].holdRepeat) {
                if (value > 0) {
                    // Immediate fire + start hold tracking
                    fireAction((Action)i);
                    double now = ImGui::GetTime();
                    heldButtons.push_back({ cc, (Action)i, now, now + NK2::HOLD_INITIAL });
                } else {
                    // Release: remove from held list
                    heldButtons.erase(
                        std::remove_if(heldButtons.begin(), heldButtons.end(),
                            [cc](const HeldButton& h) { return h.cc == cc; }),
                        heldButtons.end());
                }
            } else if (value > 0) {
                fireAction((Action)i);
            }
            return;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Note handler
    // ─────────────────────────────────────────────────────────────────────────
    void handleNoteOn(uint8_t note) {
        lastEventText = "Note " + std::to_string(note);

        // Column step-mode buttons: S=large, M=medium, R=small
        // Only consumed when the column has step sizes configured.
        // Column 8 (index 7) has no steps → its notes reach the action table.
        for (int c = 0; c < 8; c++) {
            const auto& col = columns[currentPage][c];
            bool hasSteps = (col.stepLargeHz > 0 || col.stepMediumHz > 0 || col.stepSmallHz > 0);
            if (!hasSteps) continue;
            if (col.sNote == (int)note) { colStepMode[c] = 2; return; }
            if (col.mNote == (int)note) { colStepMode[c] = 1; return; }
            if (col.rNote == (int)note) { colStepMode[c] = 0; return; }
        }

        // Transport action notes
        auto* pg = mappings[currentPage];
        for (int i = 0; i < (int)Action::Count; i++) {
            if (pg[i].note == (int)note) { fireAction((Action)i); return; }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Column slider: absolute CC position → delta-relative tune
    // ─────────────────────────────────────────────────────────────────────────
    void applyColumnSlider(int col, const ColumnMap& m, uint8_t value) {
        double step;
        switch (colStepMode[col]) {
        case 0: step = m.stepSmallHz;  break;
        case 2: step = m.stepLargeHz;  break;
        default:step = m.stepMediumHz; break;
        }
        if (step <= 0) return;
        applyRelativeTune(m.sliderCC, value, step);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Column knob: absolute CC position → zoom or volume
    // ─────────────────────────────────────────────────────────────────────────
    void applyColumnKnob(const ColumnMap& m, uint8_t value) {
        switch (m.knobFunc) {
        case KnobFunc::Zoom: {
            double totalBW = sigpath::iqFrontEnd.getSampleRate();
            double t = value / 127.0;
            gui::waterfall.setViewBandwidth(1000.0 + t * t * (totalBW - 1000.0));
            break;
        }
        case KnobFunc::Volume: {
            float vol = value / 127.0f;
            for (auto& n : sigpath::sinkManager.getStreamNames())
                sigpath::sinkManager.setStreamVolume(n, vol);
            break;
        }
        default: break;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Relative-delta tune from absolute CC slider value
    // ─────────────────────────────────────────────────────────────────────────
    void applyRelativeTune(int cc, uint8_t value, double stepHz) {
        uint8_t prev  = prevCC[cc];
        bool    known = prevCCKnown[cc];
        prevCC[cc]      = value;
        prevCCKnown[cc] = true;
        if (!known) return;
        int delta = (int)value - (int)prev;
        if (std::abs(delta) >= 64 || delta == 0) return;
        doFreqShift(delta * stepHz);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Frequency shift helper
    // ─────────────────────────────────────────────────────────────────────────
    void doFreqShift(double shiftHz) {
        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfoName = !gui::waterfall.selectedVFO.empty()
            ? gui::waterfall.selectedVFO
            : gui::waterfall.vfos.begin()->first;
        double current = gui::waterfall.getCenterFrequency()
                       + sigpath::vfoManager.getOffset(vfoName);
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfoName, std::max(0.0, current + shiftHz));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Fire transport action
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
            doFreqShift(+mappings[currentPage][(int)Action::StepTuneUp].stepHz);
            break;
        case Action::StepTuneDown:
            doFreqShift(-mappings[currentPage][(int)Action::StepTuneDown].stepHz);
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

    // ─────────────────────────────────────────────────────────────────────────
    // Action implementations
    // ─────────────────────────────────────────────────────────────────────────
    void doBandPlanStep(int direction) {
        if (!gui::waterfall.bandplan) return;
        auto& bands = gui::waterfall.bandplan->bands;
        if (bands.empty()) return;

        double center = gui::waterfall.getCenterFrequency();
        int idx = -1;
        for (int i = 0; i < (int)bands.size(); i++) {
            if (center >= bands[i].start && center <= bands[i].end) { idx = i; break; }
        }
        int next = (direction > 0)
            ? (idx < 0 ? 0 : std::min(idx + 1, (int)bands.size() - 1))
            : (idx < 0 ? (int)bands.size() - 1 : std::max(idx - 1, 0));
        if (next == idx) return;

        double target = (bands[next].start + bands[next].end) / 2.0;
        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfo = !gui::waterfall.selectedVFO.empty()
            ? gui::waterfall.selectedVFO
            : gui::waterfall.vfos.begin()->first;
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfo, target);
    }

    void doVFOCycle() {
        auto& vfos = gui::waterfall.vfos;
        if (vfos.size() <= 1) return;
        auto it = vfos.find(gui::waterfall.selectedVFO);
        if (it == vfos.end()) it = vfos.begin();
        else { ++it; if (it == vfos.end()) it = vfos.begin(); }
        gui::waterfall.selectedVFO        = it->first;
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
            for (auto& n : names)
                sigpath::sinkManager.setStreamVolume(n, muteSavedVolumes.count(n) ? muteSavedVolumes[n] : 1.0f);
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
    // Utility: format Hz value for display
    // ─────────────────────────────────────────────────────────────────────────
    static void fmtHz(char* buf, int sz, double hz) {
        if (hz <= 0)        snprintf(buf, sz, "—");
        else if (hz >= 1e9) snprintf(buf, sz, "%.0fG", hz / 1e9);
        else if (hz >= 1e6) snprintf(buf, sz, "%.0fM", hz / 1e6);
        else if (hz >= 1e3) snprintf(buf, sz, "%.0fk", hz / 1e3);
        else                snprintf(buf, sz, "%.0f",  hz);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ImGui side-panel menu
    // ─────────────────────────────────────────────────────────────────────────
    static void menuHandler(void* ctx) {
        auto* _this = reinterpret_cast<MidiControllerModule*>(ctx);
        _this->dispatchEvents();

        // ── Status row ────────────────────────────────────────────────────────
        ImGui::Text("Status: %s", _this->statusText.c_str());
        ImGui::Text("Sources: %d", _this->connectedCount);
        ImGui::Text("Last: %s",   _this->lastEventText.c_str());
        if (_this->muteActive)    { ImGui::SameLine(); ImGui::TextColored({1,0.3f,0.3f,1}, "[MUTED]"); }
        if (_this->recorderArmed) { ImGui::SameLine(); ImGui::TextColored({1,0.1f,0.1f,1}, "[REC]");   }

        // ── Page indicator ────────────────────────────────────────────────────
        ImGui::Separator();
        ImGui::Text("Page:");
        for (int p = 0; p < PAGE_COUNT; p++) {
            ImGui::SameLine();
            if (p == _this->currentPage) ImGui::TextColored({0.2f,1.0f,0.4f,1}, "[%s]", PAGE_NAMES[p]);
            else                          ImGui::TextDisabled("%s", PAGE_NAMES[p]);
        }

        // CYCLE CC assignment
        {
            char buf[24];
            snprintf(buf, sizeof(buf), _this->cycleCC >= 0 ? "CC %d" : "—", _this->cycleCC);
            ImGui::Text("CYCLE: %s", buf);
            ImGui::SameLine();
            if (_this->learningCycleCC) {
                ImGui::PushStyleColor(ImGuiCol_Button, {0.8f,0.2f,0.2f,1.0f});
                if (ImGui::SmallButton("Cancel##cyc")) _this->learningCycleCC = false;
                ImGui::PopStyleColor();
            } else {
                if (ImGui::SmallButton("Learn##cyc")) {
                    _this->learningCycleCC   = true;
                    _this->learnActionTarget = Action::Count;
                }
            }
        }
        if (_this->learningCycleCC)
            ImGui::TextColored({1,0.5f,0,1}, "Move a CC → CYCLE...");

        // ── 8-Column state dashboard ──────────────────────────────────────────
        ImGui::Separator();
        ImGui::TextUnformatted("Columns  (S=large  M=med  R=small  fn=free)");

        static const ImVec4 ACTIVE   = {0.2f,1.0f,0.4f,1.0f};
        static const ImVec4 INACTIVE = {0.45f,0.45f,0.45f,1.0f};
        static const char*  MODE_LBL[3] = {"R","M","S"};

        if (ImGui::BeginTable("##cols", 9,
                ImGuiTableFlags_Borders | ImGuiTableFlags_SizingFixedFit)) {
            ImGui::TableSetupColumn("",    ImGuiTableColumnFlags_WidthFixed, 34.0f);
            for (int c = 0; c < 8; c++) {
                char h[4]; snprintf(h, sizeof(h), "C%d", c+1);
                ImGui::TableSetupColumn(h, ImGuiTableColumnFlags_WidthFixed, 30.0f);
            }
            ImGui::TableHeadersRow();

            // Row 1: current step mode label
            ImGui::TableNextRow();
            ImGui::TableSetColumnIndex(0); ImGui::TextDisabled("mode");
            for (int c = 0; c < 8; c++) {
                ImGui::TableSetColumnIndex(c+1);
                const auto& col = _this->columns[_this->currentPage][c];
                bool hasSteps   = col.stepLargeHz > 0 || col.stepMediumHz > 0 || col.stepSmallHz > 0;
                if (hasSteps)
                    ImGui::TextColored(ACTIVE, "%s", MODE_LBL[_this->colStepMode[c]]);
                else
                    ImGui::TextColored(INACTIVE, "fn");
            }

            // Row 2: active step size value
            ImGui::TableNextRow();
            ImGui::TableSetColumnIndex(0); ImGui::TextDisabled("step");
            for (int c = 0; c < 8; c++) {
                ImGui::TableSetColumnIndex(c+1);
                const auto& col = _this->columns[_this->currentPage][c];
                bool hasSteps   = col.stepLargeHz > 0 || col.stepMediumHz > 0 || col.stepSmallHz > 0;
                if (hasSteps) {
                    double step = 0;
                    switch (_this->colStepMode[c]) {
                    case 0: step = col.stepSmallHz;  break;
                    case 2: step = col.stepLargeHz;  break;
                    default:step = col.stepMediumHz; break;
                    }
                    char sb[10]; fmtHz(sb, sizeof(sb), step);
                    ImGui::TextColored(ACTIVE, "%s", sb);
                }
            }

            // Row 3: knob function
            ImGui::TableNextRow();
            ImGui::TableSetColumnIndex(0); ImGui::TextDisabled("knob");
            for (int c = 0; c < 8; c++) {
                ImGui::TableSetColumnIndex(c+1);
                const auto& col = _this->columns[_this->currentPage][c];
                ImGui::TextColored(INACTIVE, "%s", KNOB_FUNC_NAMES[(int)col.knobFunc]);
            }

            ImGui::EndTable();
        }

        // ── Transport action mappings ─────────────────────────────────────────
        ImGui::Separator();
        ImGui::Text("Transport — %s page", PAGE_NAMES[_this->currentPage]);

        if (ImGui::BeginTable("##acts", 4,
                ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit)) {
            ImGui::TableSetupColumn("Action",   ImGuiTableColumnFlags_WidthStretch);
            ImGui::TableSetupColumn("Binding",  ImGuiTableColumnFlags_WidthFixed, 64.0f);
            ImGui::TableSetupColumn("Step",     ImGuiTableColumnFlags_WidthFixed, 72.0f);
            ImGui::TableSetupColumn("",         ImGuiTableColumnFlags_WidthFixed, 60.0f);
            ImGui::TableHeadersRow();

            for (int i = 0; i < (int)Action::Count; i++) {
                auto& m = _this->mappings[_this->currentPage][i];
                ImGui::TableNextRow();

                ImGui::TableSetColumnIndex(0);
                ImGui::TextUnformatted(ACTION_NAMES[i]);

                ImGui::TableSetColumnIndex(1);
                char bb[24];
                if (m.cc >= 0)        snprintf(bb, sizeof(bb), "CC%d%s", m.cc, m.holdRepeat ? " ↻" : "");
                else if (m.note >= 0) snprintf(bb, sizeof(bb), "N%d",    m.note);
                else                  snprintf(bb, sizeof(bb), "—");
                ImGui::TextUnformatted(bb);

                ImGui::TableSetColumnIndex(2);
                if (m.stepHz > 0) {
                    char sb[20]; fmtHz(sb, sizeof(sb), m.stepHz);
                    ImGui::TextUnformatted(sb);
                } else {
                    ImGui::TextDisabled("—");
                }

                ImGui::TableSetColumnIndex(3);
                bool isLearning = (_this->learnActionTarget == (Action)i && !_this->learningCycleCC);
                char lbl[32];
                if (isLearning) {
                    ImGui::PushStyleColor(ImGuiCol_Button, {0.8f,0.2f,0.2f,1.0f});
                    snprintf(lbl, sizeof(lbl), "Cancel##l%d", i);
                    if (ImGui::SmallButton(lbl)) _this->learnActionTarget = Action::Count;
                    ImGui::PopStyleColor();
                } else {
                    snprintf(lbl, sizeof(lbl), "Learn##l%d", i);
                    if (ImGui::SmallButton(lbl)) {
                        _this->learnActionTarget = (Action)i;
                        _this->learningCycleCC   = false;
                    }
                }
            }
            ImGui::EndTable();
        }

        if (_this->learnActionTarget != Action::Count && !_this->learningCycleCC)
            ImGui::TextColored({1,0.5f,0,1}, "Move CC or press key → '%s'",
                ACTION_NAMES[(int)_this->learnActionTarget]);

        // ── Controls ──────────────────────────────────────────────────────────
        ImGui::Separator();
        if (ImGui::Button("Reset to NK2 defaults")) {
            _this->resetToDefaults();
            _this->saveConfig();
        }
        ImGui::SameLine();
        if (ImGui::Button("Reconnect MIDI")) {
            _this->shutdownMidi();
            _this->initMidi();
        }

        // Recorder module name
        ImGui::Separator();
        ImGui::TextUnformatted("Recorder module:");
        char recBuf[64];
        snprintf(recBuf, sizeof(recBuf), "%s", _this->recorderInstanceName.c_str());
        ImGui::SetNextItemWidth(120.0f);
        if (ImGui::InputText("##rec_name", recBuf, sizeof(recBuf))) {
            _this->recorderInstanceName = recBuf;
            _this->saveConfig();
        }

        // Hold indicator
        if (!_this->heldButtons.empty())
            ImGui::TextColored({1,0.8f,0,1}, "Holding %d button(s)", (int)_this->heldButtons.size());
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
