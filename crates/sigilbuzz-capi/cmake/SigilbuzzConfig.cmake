# CMake package config for the sigilbuzz C library.
#
# `cargo cinstall -p sigilbuzz-capi` installs this file into
# `<prefix>/share/cmake/Sigilbuzz/`, next to the `sigilbuzz.pc` it writes.
# Use it with:
#
#     find_package(Sigilbuzz REQUIRED)
#     target_link_libraries(myapp PRIVATE Sigilbuzz::Sigilbuzz)
#
# The target reads its include path and link flags from `sigilbuzz.pc`,
# so the CMake and pkg-config setups always agree, and pkg-config already
# knows the right library file for each platform. That means pkg-config
# (or pkgconf) has to be installed.

include(CMakeFindDependencyMacro)
find_dependency(PkgConfig)

# This file lives in <prefix>/share/cmake/Sigilbuzz, so the install prefix
# is three levels up. Search it first, for this lookup only.
get_filename_component(_sigilbuzz_prefix "${CMAKE_CURRENT_LIST_DIR}/../../.." ABSOLUTE)
set(_sigilbuzz_saved_prefix_path "${CMAKE_PREFIX_PATH}")
set(_sigilbuzz_saved_use_prefix_path "${PKG_CONFIG_USE_CMAKE_PREFIX_PATH}")
list(INSERT CMAKE_PREFIX_PATH 0 "${_sigilbuzz_prefix}")
set(PKG_CONFIG_USE_CMAKE_PREFIX_PATH ON)

pkg_check_modules(SIGILBUZZ QUIET IMPORTED_TARGET GLOBAL sigilbuzz)

set(CMAKE_PREFIX_PATH "${_sigilbuzz_saved_prefix_path}")
set(PKG_CONFIG_USE_CMAKE_PREFIX_PATH "${_sigilbuzz_saved_use_prefix_path}")
unset(_sigilbuzz_saved_prefix_path)
unset(_sigilbuzz_saved_use_prefix_path)

if(SIGILBUZZ_FOUND)
    if(NOT TARGET Sigilbuzz::Sigilbuzz)
        add_library(Sigilbuzz::Sigilbuzz ALIAS PkgConfig::SIGILBUZZ)
    endif()
    set(Sigilbuzz_VERSION "${SIGILBUZZ_VERSION}")
    set(Sigilbuzz_FOUND TRUE)
else()
    set(Sigilbuzz_FOUND FALSE)
    set(Sigilbuzz_NOT_FOUND_MESSAGE
        "Found SigilbuzzConfig.cmake under ${_sigilbuzz_prefix}, but pkg-config could not find sigilbuzz.pc. Install pkg-config, or reinstall with cargo cinstall.")
endif()
unset(_sigilbuzz_prefix)
