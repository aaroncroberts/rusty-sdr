cmake_minimum_required(VERSION 3.13)
project(midi_controller)

file(GLOB SRC "src/*.cpp")

include(${SDRPP_MODULE_CMAKE})

target_include_directories(midi_controller PRIVATE "../../misc_modules/recorder/src")

if (APPLE)
    find_library(COREMIDI_LIB CoreMIDI REQUIRED)
    find_library(COREFOUNDATION_LIB CoreFoundation REQUIRED)
    target_link_libraries(midi_controller PRIVATE ${COREMIDI_LIB} ${COREFOUNDATION_LIB})
endif (APPLE)
