#//-----------------------------------------------------------------------------
#//	NOT BASED ON A VPC FILE (AS YOU WOULD EXPECT)
#//
#//	Project Script
#//-----------------------------------------------------------------------------
cmake_minimum_required(VERSION 3.16)

project(shaderapimtl_client)
set(SRCDIR "${CMAKE_SOURCE_DIR}")
set(CMAKE_MODULE_PATH ${SRCDIR}/cmake)
set(OUTBINNAME "shaderapimtl_client")
set(OUTBINDIR ${SRCDIR}/../game/bin)

include(${CMAKE_MODULE_PATH}/detect_platform.cmake)
include(${CMAKE_MODULE_PATH}/source_dll_base.cmake)

include_directories(../)
add_definitions(-DSHADER_DLL_EXPORT -DPROTECTED_THINGS_ENABLE)

target_compile_options(${OUTBINNAME} PRIVATE -Wno-dynamic-exception-spec)

target_sources(${OUTBINNAME} PRIVATE "../../thirdparty/metal-cpp/metal-impl.cpp")