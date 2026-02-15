#![allow(dead_code)]
pub fn encode_leb128_u32_5byte(value: u32, buf: &mut [u8; 5]) {
    *buf = leb128fmt::encode_fixed_u32(value).unwrap();
}

pub fn encode_leb128_i32_5byte(value: i32, buf: &mut [u8; 5]) {
    *buf = leb128fmt::encode_fixed_s32(value).unwrap();
}

pub fn encode_leb128_u64_10byte(value: u64, buf: &mut [u8; 10]) {
    *buf = leb128fmt::encode_fixed_u64(value).unwrap();
}

pub fn encode_leb128_i64_10byte(value: i64, buf: &mut [u8; 10]) {
    *buf = leb128fmt::encode_fixed_s64(value).unwrap();
}

pub fn encode_u32(value: u32, buf: &mut [u8; 4]) {
    *buf = value.to_le_bytes();
}

pub fn encode_u64(value: u64, buf: &mut [u8; 8]) {
    *buf = value.to_le_bytes();
}

pub fn encode_i32(value: i32, buf: &mut [u8; 4]) {
    *buf = value.to_le_bytes();
}

pub fn encode_i64(value: i64, buf: &mut [u8; 8]) {
    *buf = value.to_le_bytes();
}
