#include <imgui.h>
#include <module.h>
#include <gui/gui.h>
#include <gui/tuner.h>
#include <gui/main_window.h>
#include <signal_path/signal_path.h>
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
// MIDI event types we care about
// ─────────────────────────────────────────────────────────────────────────────
enum class MidiMsgType { CC, NoteOn, NoteOff };

struct MidiEvent {
    MidiMsgType type;
    uint8_t channel;
    uint8_t number;   // CC number or note number
    uint8_t value;    // CC value or velocity
};

// ─────────────────────────────────────────────────────────────────────────────
// Korg nanoKontrol2 default CC layout (Scene 1 / factory mapping)
//
//   Sliders (8, left-to-right):  CC 0–7
//   Knobs   (8, left-to-right):  CC 16–23
//   Transport buttons (CC, value 127=press / 0=release):
//     REW=CC43  FF=CC44  STOP=CC42  PLAY=CC41  REC=CC45  CYCLE=CC46
//   S buttons (solo):   Note 32–39
//   M buttons (mute):   Note 48–55
//   R buttons (record): Note 64–71
//
// Default SDR++ mapping:
//   Slider 1 (CC0)  → VFO coarse tune (relative delta, ±1 MHz per unit)
//   Slider 2 (CC1)  → VFO fine tune   (relative delta, ±10 kHz per unit)
//   Knob 1   (CC16) → RF gain — NOT SUPPORTED (no generic gain API in SDR++)
//   Knob 2   (CC17) → Waterfall zoom  (absolute 0–127 maps to full bandwidth)
//   PLAY     (CC41, val>0) → toggle SDR source on/off
//   STOP     (CC42, val>0) → stop SDR source
// ─────────────────────────────────────────────────────────────────────────────
namespace NK2 {
    // Sliders
    constexpr uint8_t CC_SLIDER_COARSE_TUNE = 0;   // Slider 1
    constexpr uint8_t CC_SLIDER_FINE_TUNE   = 1;   // Slider 2
    // Knobs
    constexpr uint8_t CC_KNOB_GAIN          = 16;  // Knob 1  (unsupported — no generic gain API)
    constexpr uint8_t CC_KNOB_ZOOM          = 17;  // Knob 2
    // Transport (buttons send CC; value 127 = press, 0 = release)
    constexpr uint8_t CC_PLAY              = 41;
    constexpr uint8_t CC_STOP              = 42;
    constexpr uint8_t CC_REW               = 43;
    constexpr uint8_t CC_FF                = 44;
    constexpr uint8_t CC_REC               = 45;
    // Fallback note numbers (for users who remap transport to notes)
    constexpr uint8_t NOTE_PLAY            = 41;
    constexpr uint8_t NOTE_STOP            = 42;
}

// ─────────────────────────────────────────────────────────────────────────────
// MidiControllerModule
// ─────────────────────────────────────────────────────────────────────────────
class MidiControllerModule : public ModuleManager::Instance {
public:
    MidiControllerModule(std::string name) : name(name) {
        std::fill(std::begin(prevCC), std::end(prevCC), 0);
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
    // ── CoreMIDI state ──────────────────────────────────────────────────────
    MIDIClientRef   midiClient   = 0;
    MIDIPortRef     inputPort    = 0;
    std::vector<MIDIEndpointRef> connectedSources;

    // ── Event queue (CoreMIDI thread → ImGui/main thread) ───────────────────
    std::mutex              eventMutex;
    std::vector<MidiEvent>  eventQueue;

    // ── UI state ─────────────────────────────────────────────────────────────
    std::string name;
    bool        enabled          = true;
    std::string statusText       = "Not initialised";
    std::string lastEventText    = "—";
    int         connectedCount   = 0;

    // ── Frequency step sizes (Hz) for knob/slider mapping ────────────────────
    // Knob 0 (CC16): coarse tune ±1 MHz steps
    // Knob 1 (CC17): fine tune ±10 kHz steps
    // Slider 0 (CC0): absolute zoom (maps 0–127 → full bandwidth range)
    static constexpr double COARSE_STEP_HZ = 1e6;
    static constexpr double FINE_STEP_HZ   = 10e3;

    // ── Previous CC values for relative delta detection ──────────────────
    // Initialised to 255 (sentinel: "not yet received") so the very first
    // event from a slider/knob doesn't produce a spurious large delta.
    uint8_t prevCC[128];
    bool prevCCKnown[128] = {};

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
        for (auto src : connectedSources) {
            MIDIPortDisconnectSource(inputPort, src);
        }
        connectedSources.clear();
        connectedCount = 0;

        if (inputPort)  { MIDIPortDispose(inputPort);   inputPort  = 0; }
        if (midiClient) { MIDIClientDispose(midiClient); midiClient = 0; }
        statusText = "Disconnected";
    }

    // Connect to every source whose name contains "nanoKONTROL" (case-insensitive).
    // Falls back to connecting everything if no nanoKONTROL is found.
    void connectAllSources() {
        ItemCount n = MIDIGetNumberOfSources();
        if (n == 0) {
            statusText = "No MIDI sources found";
            flog::warn("MidiController: no MIDI sources available");
            return;
        }

        bool foundNK2 = false;
        for (ItemCount i = 0; i < n; i++) {
            MIDIEndpointRef src = MIDIGetSource(i);
            std::string devName = endpointName(src);

            // Match nanoKONTROL2 (or any nanoKontrol variant)
            std::string lower = devName;
            std::transform(lower.begin(), lower.end(), lower.begin(), ::tolower);
            bool isNK2 = lower.find("nanokontrol") != std::string::npos;

            if (isNK2) {
                connectSource(src, devName);
                foundNK2 = true;
            }
        }

        if (!foundNK2) {
            flog::warn("MidiController: nanoKONTROL2 not found — connecting all {} source(s)", (int)n);
            for (ItemCount i = 0; i < n; i++) {
                MIDIEndpointRef src = MIDIGetSource(i);
                connectSource(src, endpointName(src));
            }
        }

        connectedCount = (int)connectedSources.size();
        statusText = connectedCount > 0
            ? "Connected (" + std::to_string(connectedCount) + " source" + (connectedCount > 1 ? "s)" : ")")
            : "No matching sources";
    }

    void connectSource(MIDIEndpointRef src, const std::string& devName) {
        OSStatus st = MIDIPortConnectSource(inputPort, src, nullptr);
        if (st == noErr) {
            connectedSources.push_back(src);
            flog::info("MidiController: connected to '{}'", devName);
        } else {
            flog::error("MidiController: failed to connect '{}' ({})", devName, (int)st);
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
                             void* readProcRefCon,
                             void* /*srcConnRefCon*/) {
        auto* self = reinterpret_cast<MidiControllerModule*>(readProcRefCon);
        const MIDIPacket* pkt = &pktList->packet[0];

        std::lock_guard<std::mutex> lk(self->eventMutex);
        for (UInt32 i = 0; i < pktList->numPackets; i++) {
            if (pkt->length >= 3) {
                uint8_t status  = pkt->data[0] & 0xF0;
                uint8_t channel = pkt->data[0] & 0x0F;
                uint8_t num     = pkt->data[1];
                uint8_t val     = pkt->data[2];

                MidiEvent ev{};
                ev.channel = channel;
                ev.number  = num;
                ev.value   = val;

                if (status == 0xB0) {       // CC
                    ev.type = MidiMsgType::CC;
                    self->eventQueue.push_back(ev);
                } else if (status == 0x90) { // Note On
                    ev.type = MidiMsgType::NoteOn;
                    self->eventQueue.push_back(ev);
                } else if (status == 0x80) { // Note Off
                    ev.type = MidiMsgType::NoteOff;
                    self->eventQueue.push_back(ev);
                }
            }
            pkt = MIDIPacketNext(pkt);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Dispatch queued MIDI events → SDR++ API calls
    // Called from ImGui render thread (safe for all SDR++ APIs we use)
    // ─────────────────────────────────────────────────────────────────────────
    void dispatchEvents() {
        std::vector<MidiEvent> local;
        {
            std::lock_guard<std::mutex> lk(eventMutex);
            local.swap(eventQueue);
        }

        for (auto& ev : local) {
            if (ev.type == MidiMsgType::CC) {
                handleCC(ev.number, ev.value);
            } else if (ev.type == MidiMsgType::NoteOn && ev.value > 0) {
                handleNote(ev.number);
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CC handler: nanoKontrol2 default mapping
    // ─────────────────────────────────────────────────────────────────────────
    void handleCC(uint8_t cc, uint8_t value) {
        uint8_t prev = prevCC[cc];
        bool known = prevCCKnown[cc];
        prevCC[cc] = value;
        prevCCKnown[cc] = true;

        lastEventText = "CC " + std::to_string(cc) + " = " + std::to_string(value);

        // ── Transport: PLAY (CC41) ────────────────────────────────────────────
        // nanoKontrol2 sends value 127 on press, 0 on release — act on press only
        if (cc == NK2::CC_PLAY) {
            if (value > 0) {
                bool running = gui::mainWindow.sdrIsRunning();
                gui::mainWindow.setPlayState(!running);
            }
            return;
        }

        // ── Transport: STOP (CC42) ────────────────────────────────────────────
        if (cc == NK2::CC_STOP) {
            if (value > 0 && gui::mainWindow.sdrIsRunning()) {
                gui::mainWindow.setPlayState(false);
            }
            return;
        }

        // ── Coarse tune: Slider 1 (CC0) — relative delta ─────────────────────
        // Sliders send absolute 0–127. We compare to the previous value and treat
        // the difference as a step count. Large jumps (≥64) are filtered as
        // "slider was at an unknown position on connect" artefacts.
        if (cc == NK2::CC_SLIDER_COARSE_TUNE) {
            if (known) {
                int delta = (int)value - (int)prev;
                if (std::abs(delta) < 64 && delta != 0) {
                    adjustFrequency(delta * COARSE_STEP_HZ);
                }
            }
            return;
        }

        // ── Fine tune: Slider 2 (CC1) — relative delta ───────────────────────
        if (cc == NK2::CC_SLIDER_FINE_TUNE) {
            if (known) {
                int delta = (int)value - (int)prev;
                if (std::abs(delta) < 64 && delta != 0) {
                    adjustFrequency(delta * FINE_STEP_HZ);
                }
            }
            return;
        }

        // ── Knob 1 (CC16): RF gain — not supported in SDR++ v1 ───────────────
        // SDR++ has no generic gain API; gain is per-source hardware only.
        // Log the event so users can verify the knob is being received.
        if (cc == NK2::CC_KNOB_GAIN) {
            // RF gain control not available — no generic SourceManager gain API
            return;
        }

        // ── Waterfall zoom: Knob 2 (CC17) ────────────────────────────────────
        // Map 0–127 linearly to t in [0,1], then quadratic → bandwidth for
        // a perceptually linear zoom feel (wide range at bottom, fine at top).
        if (cc == NK2::CC_KNOB_ZOOM) {
            double totalBW = sigpath::iqFrontEnd.getSampleRate();
            double t = value / 127.0;
            double bw = 1000.0 + (t * t * (totalBW - 1000.0));
            gui::waterfall.setViewBandwidth(bw);
            return;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Note handler: transport buttons
    // ─────────────────────────────────────────────────────────────────────────
    // Note handler: fallback for users who remap nanoKontrol2 transport to notes
    void handleNote(uint8_t note) {
        lastEventText = "Note " + std::to_string(note);

        if (note == NK2::NOTE_PLAY) {
            bool running = gui::mainWindow.sdrIsRunning();
            gui::mainWindow.setPlayState(!running);
            return;
        }
        if (note == NK2::NOTE_STOP) {
            if (gui::mainWindow.sdrIsRunning()) {
                gui::mainWindow.setPlayState(false);
            }
            return;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Tune by a delta in Hz relative to current VFO position
    // ─────────────────────────────────────────────────────────────────────────
    void adjustFrequency(double deltaHz) {
        // Use the first VFO found in the waterfall (typically "Radio")
        if (gui::waterfall.vfos.empty()) return;
        const std::string& vfo = gui::waterfall.vfos.begin()->first;

        double center  = gui::waterfall.getCenterFrequency();
        double offset  = sigpath::vfoManager.getOffset(vfo);
        double current = center + offset;
        double newFreq = current + deltaHz;
        if (newFreq < 0) newFreq = 0;

        tuner::tune(tuner::TUNER_MODE_NORMAL, vfo, newFreq);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ImGui side-panel menu
    // ─────────────────────────────────────────────────────────────────────────
    static void menuHandler(void* ctx) {
        auto* _this = reinterpret_cast<MidiControllerModule*>(ctx);
        _this->dispatchEvents();

        ImGui::Text("Status: %s", _this->statusText.c_str());
        ImGui::Text("Connected sources: %d", _this->connectedCount);
        ImGui::Separator();
        ImGui::Text("Last event: %s", _this->lastEventText.c_str());
        ImGui::Separator();
        ImGui::TextDisabled("nanoKONTROL2 default mapping:");
        ImGui::TextDisabled("  CC0  Slider 1 → Coarse tune (1 MHz/step)");
        ImGui::TextDisabled("  CC1  Slider 2 → Fine tune (10 kHz/step)");
        ImGui::TextDisabled("  CC16 Knob 1   → RF gain (not supported)");
        ImGui::TextDisabled("  CC17 Knob 2   → Waterfall zoom");
        ImGui::TextDisabled("  CC41 PLAY     → Start/stop SDR toggle");
        ImGui::TextDisabled("  CC42 STOP     → Stop SDR");

        if (ImGui::Button("Reconnect MIDI")) {
            _this->shutdownMidi();
            _this->initMidi();
        }
    }
};

MOD_EXPORT void _INIT_() {}

MOD_EXPORT ModuleManager::Instance* _CREATE_INSTANCE_(std::string name) {
    return new MidiControllerModule(name);
}

MOD_EXPORT void _DELETE_INSTANCE_(void* instance) {
    delete (MidiControllerModule*)instance;
}

MOD_EXPORT void _END_() {}
