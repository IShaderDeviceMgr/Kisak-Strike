#//-----------------------------------------------------------------------------
#//	LAUNCHER_MAIN.VPC
#//
#//	Project Script
#//-----------------------------------------------------------------------------
cmake_minimum_required(VERSION 3.16)
#dont build this on dedicated build
project(vbspinfo)
set(SRCDIR "${CMAKE_SOURCE_DIR}")
set(CMAKE_MODULE_PATH ${SRCDIR}/cmake)
set(OUTBINDIR "${SRCDIR}/../game/bin")

include(${CMAKE_MODULE_PATH}/detect_platform.cmake)

set(OUTBINNAME "vbspinfo")

set(NOSTINKYLINKIES "1")
set(NOSKELETONBASE "1")

include(${CMAKE_MODULE_PATH}/source_exe_base.cmake)

find_package(Iconv REQUIRED)

target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/utils/common/bsplib.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/utils/common/cmdlib.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/utils/common/scriplib.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/utils/common/filesystem_tools.cpp)


target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/public/filesystem_helpers.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/public/filesystem_init.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/filesystem/linux_support.cpp)


target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/public/zip_utils.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/public/scratchpad3d.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/public/collisionutils.cpp)
target_sources(${OUTBINNAME} PRIVATE ${SRCDIR}/public/lumpfiles.cpp)

target_sources(${OUTBINNAME} PRIVATE vbspinfo.cpp)

include_directories(${SRCDIR}/utils/common)
include_directories(${SRCDIR}/common)
include_directories(${SRCDIR}/public/tier1)

target_link_libraries(${OUTBINNAME} libtier0_client tier1_client tier2_client vstdlib_client Iconv::Iconv mathlib_client)

