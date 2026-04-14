cmake_minimum_required(VERSION 3.13)
project(midi_controller)

file(GLOB SRC "src/*.cpp")

include(${SDRPP_MODULE_CMAKE})

if (APPLE)
    target_link_libraries(midi_controller PRIVATE "-framework CoreMIDI" "-framework CoreFoundation")
endif (APPLE)
