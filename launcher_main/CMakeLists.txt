#//-----------------------------------------------------------------------------
#//	LAUNCHER_MAIN.VPC
#//
#//	Project Script
#//-----------------------------------------------------------------------------
cmake_minimum_required(VERSION 3.16)
#dont build this on dedicated build
project(launcher_main)
set(SRCDIR "${CMAKE_SOURCE_DIR}")
set(CMAKE_MODULE_PATH ${SRCDIR}/cmake)
set(OUTBINDIR "${SRCDIR}/../game")

include(${CMAKE_MODULE_PATH}/detect_platform.cmake)

# Kisak-Strike only targets POSIX platforms (Linux and macOS today, see
# ../PORTING.md) -- no Windows, no consoles.
if( NOT (LINUXALL OR OSXALL) )
    message(FATAL_ERROR "launcher_main only supports Linux and macOS.")
endif()

if( OSX32 )
    set(OUTBINNAME "csgo_osx")
elseif( OSX64 )
    set(OUTBINNAME "csgo_osx64")
elseif( LINUX32 )
    set(OUTBINNAME "csgo_linux")
elseif( LINUX64 )
    set(OUTBINNAME "csgo_linux64")
endif()

# launcher_main is a Rust crate (src/main.rs) -- see ../PORTING.md. It's a
# standalone dlopen()+dlsym() redirection stub with no dependency on any
# other module's ABI, so it needed no cxx bridge or vtable shim; this just
# runs `cargo build` and drops the resulting binary in OUTBINDIR.
if( CMAKE_BUILD_TYPE STREQUAL "DEBUG" )
    set( CARGO_BUILD_FLAG "" )
    set( CARGO_PROFILE_DIR "debug" )
else()
    set( CARGO_BUILD_FLAG "--release" )
    set( CARGO_PROFILE_DIR "release" )
endif()

set( CARGO_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/target" )
set( RUST_LAUNCHER_MAIN_BIN "${CARGO_TARGET_DIR}/${CARGO_PROFILE_DIR}/launcher_main" )

add_custom_command(
    OUTPUT "${RUST_LAUNCHER_MAIN_BIN}"
    COMMAND ${CMAKE_COMMAND} -E env "CARGO_TARGET_DIR=${CARGO_TARGET_DIR}"
            cargo build ${CARGO_BUILD_FLAG}
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/src/main.rs" "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml"
    COMMENT "Building launcher_main (Rust)"
    VERBATIM
)

add_custom_target( ${OUTBINNAME}_cargo ALL DEPENDS "${RUST_LAUNCHER_MAIN_BIN}" )

add_custom_command(
    TARGET ${OUTBINNAME}_cargo POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E make_directory "${OUTBINDIR}"
    COMMAND ${CMAKE_COMMAND} -E copy "${RUST_LAUNCHER_MAIN_BIN}" "${OUTBINDIR}/${OUTBINNAME}"
    VERBATIM
)
