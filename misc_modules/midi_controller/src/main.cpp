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
// Nanocontrol2 CC / note layout
//   Knobs:    CC 16–23  (8 knobs, top row)
//   Sliders:  CC  0–7   (8 sliders)
//   Transport buttons: Note 41 (Rwd), 42 (Fwd), 43 (Stop), 44 (Play), 45 (Rec)
//   S buttons: Note 32–39  Mute buttons: Note 48–55  Solo: Note 64–71
// ─────────────────────────────────────────────────────────────────────────────
namespace NK2 {
    constexpr uint8_t SLIDER_BASE = 0;
    constexpr uint8_t KNOB_BASE   = 16;
    constexpr uint8_t NOTE_PLAY   = 41;
    constexpr uint8_t NOTE_STOP   = 43;
    constexpr uint8_t NOTE_FWD    = 42;
    constexpr uint8_t NOTE_RWD    = 41;
}

// ─────────────────────────────────────────────────────────────────────────────
// MidiControllerModule
// ─────────────────────────────────────────────────────────────────────────────
class MidiControllerModule : public ModuleManager::Instance {
public:
    MidiControllerModule(std::string name) : name(name) {
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

    // ── Previous CC values for relative delta detection ────────────────────
    uint8_t prevCC[128] = {};

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
        prevCC[cc] = value;

        lastEventText = "CC " + std::to_string(cc) + " = " + std::to_string(value);

        // ── Waterfall zoom: Slider 0 (CC 0) ─────────────────────────────────
        // Map 0–127 linearly to t in [0,1], then quadratic → bandwidth
        if (cc == NK2::SLIDER_BASE) {
            double totalBW = sigpath::iqFrontEnd.getSampleRate();
            double t = value / 127.0;
            double bw = 1000.0 + (t * t * (totalBW - 1000.0));
            gui::waterfall.setViewBandwidth(bw);
            return;
        }

        // ── Coarse tune: Knob 0 (CC 16) — relative, centre-detent ───────────
        // nanoKontrol2 knobs send 0–127 as absolute position.
        // We treat them as relative: delta = (value - prev), skipping large jumps
        // (which indicate the knob wrapped or the controller was just connected).
        if (cc == NK2::KNOB_BASE) {
            int delta = (int)value - (int)prev;
            if (std::abs(delta) < 64) { // ignore wrap-around artefacts
                if (delta != 0) {
                    adjustFrequency(delta * COARSE_STEP_HZ);
                }
            }
            return;
        }

        // ── Fine tune: Knob 1 (CC 17) ────────────────────────────────────────
        if (cc == NK2::KNOB_BASE + 1) {
            int delta = (int)value - (int)prev;
            if (std::abs(delta) < 64) {
                if (delta != 0) {
                    adjustFrequency(delta * FINE_STEP_HZ);
                }
            }
            return;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Note handler: transport buttons
    // ─────────────────────────────────────────────────────────────────────────
    void handleNote(uint8_t note) {
        lastEventText = "Note " + std::to_string(note);

        // Play/Stop toggle
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
        ImGui::TextDisabled("nanoKontrol2 default mapping:");
        ImGui::TextDisabled("  CC0  Slider 0 → Zoom");
        ImGui::TextDisabled("  CC16 Knob 0  → Coarse tune (1 MHz/step)");
        ImGui::TextDisabled("  CC17 Knob 1  → Fine tune (10 kHz/step)");
        ImGui::TextDisabled("  Play button  → Start/stop SDR");
        ImGui::TextDisabled("  Stop button  → Stop SDR");

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
