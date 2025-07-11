pub fn encode_leb128_u32_5byte(mut value: u32, buf: &mut [u8; 5]) {
    for i in 0..5 {
        buf[i] = (value as u8) & 0x7f;
        value >>= 7;
    }
    for i in 0..4 {
        buf[i] |= 0x80;
    }
}

pub fn encode_leb128_i32_5byte(mut value: i32, buf: &mut [u8; 5]) {
    for i in 0..5 {
        buf[i] = (value as u8) & 0x7f;
        value >>= 7;
    }
    for i in 0..4 {
        buf[i] |= 0x80;
    }
}

pub fn encode_leb128_u64_10byte(mut value: u64, buf: &mut [u8; 10]) {
    for i in 0..10 {
        buf[i] = (value as u8) & 0x7f;
        value >>= 7;
    }
    for i in 0..9 {
        buf[i] |= 0x80;
    }
}

pub fn encode_leb128_i64_10byte(mut value: i64, buf: &mut [u8; 10]) {
    for i in 0..10 {
        buf[i] = (value as u8) & 0x7f;
        value >>= 7;
    }
    for i in 0..9 {
        buf[i] |= 0x80;
    }
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
