#include <imgui.h>
#include <module.h>
#include <gui/gui.h>
#include <utils/flog.h>

SDRPP_MOD_INFO{
    /* Name:            */ "midi_controller",
    /* Description:     */ "MIDI controller integration (CoreMIDI) for SDR++ — tune, zoom, and transport via hardware knobs/sliders",
    /* Author:          */ "Aaron C. Roberts",
    /* Version:         */ 0, 1, 0,
    /* Max instances    */ 1
};

class MidiControllerModule : public ModuleManager::Instance {
public:
    MidiControllerModule(std::string name) {
        this->name = name;
        gui::menu.registerEntry(name, menuHandler, this, NULL);
        flog::info("MidiControllerModule '{}': loaded", name);
    }

    ~MidiControllerModule() {
        gui::menu.removeEntry(name);
    }

    void postInit() {}

    void enable() {
        enabled = true;
    }

    void disable() {
        enabled = false;
    }

    bool isEnabled() {
        return enabled;
    }

private:
    static void menuHandler(void* ctx) {
        MidiControllerModule* _this = (MidiControllerModule*)ctx;
        ImGui::Text("MIDI Controller: %s", _this->name.c_str());
        ImGui::Text("Status: %s", _this->enabled ? "Enabled" : "Disabled");
        ImGui::Separator();
        ImGui::TextDisabled("CoreMIDI integration coming soon.");
    }

    std::string name;
    bool enabled = true;
};

MOD_EXPORT void _INIT_() {}

MOD_EXPORT ModuleManager::Instance* _CREATE_INSTANCE_(std::string name) {
    return new MidiControllerModule(name);
}

MOD_EXPORT void _DELETE_INSTANCE_(void* instance) {
    delete (MidiControllerModule*)instance;
}

MOD_EXPORT void _END_() {}
