// C++ wrapper for bnsh-decoder to expose FFI-friendly interface for Rust

#pragma once

#include <cstdint>
#include <vector>
#include <string>
#include <map>

// SPIR-V format constants
const uint32_t SPIRV_MAGIC = 0x07230203;
const uint32_t SPIRV_VERSION = 0x00010000;

/// Decode a BNSH shader binary to SPIR-V
/// Input: bnsh_binary - raw BNSH shader binary data
/// Input: bnsh_size - size of BNSH binary in bytes
/// Output: spirv_out_size - on input, capacity in u32s; on output, actual count written
/// Output: entry_point_len - on output, length of entry point string (excluding null)
/// Output: stage_out - shader stage ID (0=Fragment, 1=Vertex, etc)
/// Output: sampler_count_out - number of samplers
/// Output: uniform_buffer_count_out - number of uniform buffers
/// Returns: true if decode succeeded, false otherwise
bool decode_bnsh_to_spirv(
    const uint8_t* bnsh_binary,
    size_t bnsh_size,
    uint32_t* spirv_out,
    size_t& spirv_out_size,
    uint8_t* entry_point_out,
    size_t& entry_point_len,
    uint32_t& stage_out,
    uint32_t& sampler_count_out,
    uint32_t& uniform_buffer_count_out
);

/// Get shader stage name from stage ID
const char* get_shader_stage_name(uint32_t stage);
