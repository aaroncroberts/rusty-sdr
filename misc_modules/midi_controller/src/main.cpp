#include <imgui.h>
#include <module.h>
#include <gui/gui.h>
#include <gui/tuner.h>
#include <gui/main_window.h>
#include <signal_path/signal_path.h>
#include <core.h>
#include <config.h>
#include <utils/flog.h>

#include <CoreMIDI/CoreMIDI.h>
#include <CoreFoundation/CoreFoundation.h>

#include <mutex>
#include <vector>
#include <string>
#include <algorithm>
#include <cmath>

SDRPP_MOD_INFO{
    /* Name:            */ "midi_controller",
    /* Description:     */ "MIDI controller integration (CoreMIDI) for SDR++ — tune, zoom, and transport via hardware knobs/sliders",
    /* Author:          */ "Aaron C. Roberts",
    /* Version:         */ 0, 1, 0,
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
// Korg nanoKontrol2 Scene 1 factory CC layout — used for "Reset to defaults"
//
//   Sliders (left→right): CC 0–7
//   Knobs   (left→right): CC 16–23
//   Transport (CC, value 127=press / 0=release):
//     REW=43  FF=44  STOP=42  PLAY=41  REC=45  CYCLE=46
//   S/M/R buttons send Notes: S=32–39, M=48–55, R=64–71
//
// Default SDR++ assignment:
//   Slider 1 (CC0)  → VFO coarse tune (±1 MHz/unit, relative delta)
//   Slider 2 (CC1)  → VFO fine tune   (±10 kHz/unit, relative delta)
//   Knob 1   (CC16) → RF gain         (not supported — no generic gain API)
//   Knob 2   (CC17) → Waterfall zoom  (absolute 0–127 → full bandwidth range)
//   PLAY     (CC41) → toggle SDR source on/off
//   STOP     (CC42) → stop SDR source
// ─────────────────────────────────────────────────────────────────────────────
namespace NK2Defaults {
    constexpr int CC_TUNE_COARSE = 0;
    constexpr int CC_TUNE_FINE   = 1;
    constexpr int CC_GAIN        = 16;  // unsupported
    constexpr int CC_ZOOM        = 17;
    constexpr int CC_PLAY        = 41;
    constexpr int CC_STOP        = 42;
    constexpr double STEP_COARSE_HZ = 1e6;
    constexpr double STEP_FINE_HZ   = 10e3;
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-action MIDI mapping
// ─────────────────────────────────────────────────────────────────────────────
struct ActionMap {
    int  cc      = -1;    // CC number, -1 = unassigned
    int  channel = -1;    // MIDI channel filter, -1 = any
    double stepHz = 0;    // for continuous tune actions only
};

// Actions the user can bind
enum class Action {
    TuneCoarse,
    TuneFine,
    Zoom,
    Play,
    Stop,
    Count
};

static const char* ACTION_NAMES[] = {
    "Tune Coarse", "Tune Fine", "Zoom", "Play/Toggle", "Stop"
};

// ─────────────────────────────────────────────────────────────────────────────
// MidiControllerModule
// ─────────────────────────────────────────────────────────────────────────────
class MidiControllerModule : public ModuleManager::Instance {
public:
    MidiControllerModule(std::string name) : name(name) {
        // Load or initialise config defaults
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

    void postInit() {
        initMidi();
    }

    void enable() {
        enabled = true;
        if (midiClient == 0) initMidi();
    }

    void disable() {
        enabled = false;
        shutdownMidi();
    }

    bool isEnabled() {
        return enabled;
    }

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

    // ── MIDI learn state ──────────────────────────────────────────────────────
    // learnTarget = Action::Count → not learning
    Action learnTarget = Action::Count;

    // ── UI display state ──────────────────────────────────────────────────────
    std::string statusText    = "Not initialised";
    std::string lastEventText = "—";
    int         connectedCount = 0;

    // ─────────────────────────────────────────────────────────────────────────
    // Config load / save
    // ─────────────────────────────────────────────────────────────────────────
    void setDefaultMappings() {
        mappings[(int)Action::TuneCoarse] = { NK2Defaults::CC_TUNE_COARSE, -1, NK2Defaults::STEP_COARSE_HZ };
        mappings[(int)Action::TuneFine]   = { NK2Defaults::CC_TUNE_FINE,   -1, NK2Defaults::STEP_FINE_HZ   };
        mappings[(int)Action::Zoom]       = { NK2Defaults::CC_ZOOM,        -1, 0 };
        mappings[(int)Action::Play]       = { NK2Defaults::CC_PLAY,        -1, 0 };
        mappings[(int)Action::Stop]       = { NK2Defaults::CC_STOP,        -1, 0 };
    }

    void loadConfig() {
        auto& cfg = config.conf[name];
        auto load = [&](const char* key, Action a) {
            if (cfg.contains(key)) {
                auto& m = cfg[key];
                mappings[(int)a].cc      = m.value("cc", -1);
                mappings[(int)a].channel = m.value("channel", -1);
                mappings[(int)a].stepHz  = m.value("stepHz", 0.0);
            }
        };
        load("tuneCoarse", Action::TuneCoarse);
        load("tuneFine",   Action::TuneFine);
        load("zoom",       Action::Zoom);
        load("play",       Action::Play);
        load("stop",       Action::Stop);
    }

    void saveConfig() {
        auto save = [&](const char* key, Action a) {
            auto& m = mappings[(int)a];
            config.conf[name][key]["cc"]      = m.cc;
            config.conf[name][key]["channel"] = m.channel;
            config.conf[name][key]["stepHz"]  = m.stepHz;
        };
        save("tuneCoarse", Action::TuneCoarse);
        save("tuneFine",   Action::TuneFine);
        save("zoom",       Action::Zoom);
        save("play",       Action::Play);
        save("stop",       Action::Stop);
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
            if (ev.type == MidiMsgType::CC) {
                // MIDI learn: capture the first CC event and assign it
                if (learnTarget != Action::Count && ev.value > 0) {
                    mappings[(int)learnTarget].cc      = ev.number;
                    mappings[(int)learnTarget].channel = -1; // accept any channel
                    saveConfig();
                    learnTarget = Action::Count;
                    lastEventText = "Learned: CC " + std::to_string(ev.number);
                    continue;
                }
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

        if (cc == (uint8_t)mappings[(int)Action::Play].cc && value > 0) {
            gui::mainWindow.setPlayState(!gui::mainWindow.sdrIsRunning());
            return;
        }
        if (cc == (uint8_t)mappings[(int)Action::Stop].cc && value > 0) {
            if (gui::mainWindow.sdrIsRunning()) gui::mainWindow.setPlayState(false);
            return;
        }
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
    }

    // Fallback: handle note-mapped transport (for users who remap nanoKontrol2)
    void handleNoteOn(uint8_t note) {
        lastEventText = "Note " + std::to_string(note);
        if (note == 41) { gui::mainWindow.setPlayState(!gui::mainWindow.sdrIsRunning()); return; }
        if (note == 42) { if (gui::mainWindow.sdrIsRunning()) gui::mainWindow.setPlayState(false); return; }
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
        if (std::abs(delta) >= 64 || delta == 0) return;  // filter wrap-around

        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfo = gui::waterfall.vfos.begin()->first;
        double current = gui::waterfall.getCenterFrequency() + sigpath::vfoManager.getOffset(vfo);
        double newFreq = std::max(0.0, current + delta * stepHz);
        tuner::tune(tuner::TUNER_MODE_NORMAL, vfo, newFreq);
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

        ImGui::Separator();
        ImGui::TextUnformatted("MIDI Mapping");

        // Table: Action | CC | Step | Learn button
        if (ImGui::BeginTable("##midi_map", 4,
                ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit)) {
            ImGui::TableSetupColumn("Action",  ImGuiTableColumnFlags_WidthStretch);
            ImGui::TableSetupColumn("CC",      ImGuiTableColumnFlags_WidthFixed, 40.0f);
            ImGui::TableSetupColumn("Step Hz", ImGuiTableColumnFlags_WidthFixed, 80.0f);
            ImGui::TableSetupColumn("",        ImGuiTableColumnFlags_WidthFixed, 55.0f);
            ImGui::TableHeadersRow();

            for (int i = 0; i < (int)Action::Count; i++) {
                auto& m = _this->mappings[i];
                ImGui::TableNextRow();

                ImGui::TableSetColumnIndex(0);
                ImGui::TextUnformatted(ACTION_NAMES[i]);

                ImGui::TableSetColumnIndex(1);
                char ccBuf[8];
                if (m.cc < 0) snprintf(ccBuf, sizeof(ccBuf), "—");
                else snprintf(ccBuf, sizeof(ccBuf), "%d", m.cc);
                ImGui::TextUnformatted(ccBuf);

                ImGui::TableSetColumnIndex(2);
                if (m.stepHz > 0) {
                    char stepBuf[32];
                    if (m.stepHz >= 1e6) snprintf(stepBuf, sizeof(stepBuf), "%.0f MHz", m.stepHz / 1e6);
                    else if (m.stepHz >= 1e3) snprintf(stepBuf, sizeof(stepBuf), "%.0f kHz", m.stepHz / 1e3);
                    else snprintf(stepBuf, sizeof(stepBuf), "%.0f Hz", m.stepHz);
                    ImGui::TextUnformatted(stepBuf);
                } else {
                    ImGui::TextDisabled("—");
                }

                ImGui::TableSetColumnIndex(3);
                bool isLearning = (_this->learnTarget == (Action)i);
                if (isLearning) {
                    ImGui::PushStyleColor(ImGuiCol_Button, ImVec4(0.8f, 0.2f, 0.2f, 1.0f));
                    char btnLabel[32];
                    snprintf(btnLabel, sizeof(btnLabel), "Cancel##lrn%d", i);
                    if (ImGui::SmallButton(btnLabel)) {
                        _this->learnTarget = Action::Count;
                    }
                    ImGui::PopStyleColor();
                } else {
                    char btnLabel[32];
                    snprintf(btnLabel, sizeof(btnLabel), "Learn##lrn%d", i);
                    if (ImGui::SmallButton(btnLabel)) {
                        _this->learnTarget = (Action)i;
                    }
                }
            }
            ImGui::EndTable();
        }

        if (_this->learnTarget != Action::Count) {
            ImGui::TextColored(ImVec4(1, 0.5f, 0, 1), "Move a control to assign it to '%s'",
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
