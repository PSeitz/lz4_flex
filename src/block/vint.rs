

/// Decode a `vint32`-encoded unsigned 32-bit integer from a bytes slice.
/// Maximum space required are 5 bytes.
/// `pos` will be used to access the pos in the slice.
/// `pos` will be incremented by the number of bytes used to dencode the u32.
///
/// Will panic if incorrectly encoded.
#[inline]
#[cfg(feature = "safe-encode")]
pub fn decode_varint_slice(data: &[u8], pos: &mut usize) -> u32 {
    let next = data[*pos];
    *pos += 1;
    let mut ret: u32 = (next as u32) & 127;
    if next & 128 == 0 {
        return ret;
    }
    let next = data[*pos];
    *pos += 1;
    ret |= ((next as u32) & 127) << 7;
    if next & 128 == 0 {
        return ret;
    }
    let next = data[*pos];
    *pos += 1;
    ret |= ((next as u32) & 127) << 14;
    if next & 128 == 0 {
        return ret;
    }
    let next = data[*pos];
    *pos += 1;
    ret |= ((next as u32) & 127) << 21;
    if next & 128 == 0 {
        return ret;
    }
    let next = data[*pos];
    *pos += 1;
    ret |= ((next as u32) & 127) << 28;
    ret
}
/// Decode a `vint32`-encoded unsigned 32-bit integer from a bytes slice.
/// Maximum space required are 5 bytes.
/// `pos` will be used to access the pos in the slice.
/// `pos` will be incremented by the number of bytes used to dencode the u32.
///
/// Will panic if incorrectly encoded.
#[inline]
#[cfg(not(feature = "safe-encode"))]
pub fn decode_varint_slice(data: &[u8], pos: &mut usize) -> u32 {
    let next = unsafe{*data.get_unchecked(*pos)};
    *pos += 1;
    let mut ret: u32 = (next as u32) & 127;
    if next & 128 == 0 {
        return ret;
    }
    let next = unsafe{*data.get_unchecked(*pos)};
    *pos += 1;
    ret |= ((next as u32) & 127) << 7;
    if next & 128 == 0 {
        return ret;
    }
    let next = unsafe{*data.get_unchecked(*pos)};
    *pos += 1;
    ret |= ((next as u32) & 127) << 14;
    if next & 128 == 0 {
        return ret;
    }
    let next = unsafe{*data.get_unchecked(*pos)};
    *pos += 1;
    ret |= ((next as u32) & 127) << 21;
    if next & 128 == 0 {
        return ret;
    }
    let next = unsafe{*data.get_unchecked(*pos)};
    *pos += 1;
    ret |= ((next as u32) & 127) << 28;
    ret
}


/// `vint32` encode a unsigned 32-bit integer into a vec.
///
/// returns number of bytes written
///
#[inline]
pub fn encode_varint_into(output: &mut alloc::vec::Vec<u8>, mut value: u32) -> u8 {
    let do_one = |output: &mut alloc::vec::Vec<u8>, value: &mut u32| {
        push_byte(output, ((*value & 127) | 128) as u8);
        *value >>= 7;
    };
    let do_last = |output: &mut alloc::vec::Vec<u8>, value: u32| {
        push_byte(output, (value & 127) as u8);
    };

    if value < 1 << 7 {
        //128
        do_last(output, value);
        1
    } else if value < 1 << 14 {
        do_one(output, &mut value);
        do_last(output, value);
        2
    } else if value < 1 << 21 {
        do_one(output, &mut value);
        do_one(output, &mut value);
        do_last(output, value);
        3
    } else if value < 1 << 28 {
        do_one(output, &mut value);
        do_one(output, &mut value);
        do_one(output, &mut value);
        do_last(output, value);
        4
    } else {
        do_one(output, &mut value);
        do_one(output, &mut value);
        do_one(output, &mut value);
        do_one(output, &mut value);
        do_last(output, value);
        5
    }
}

#[inline]
#[cfg(feature = "safe-encode")]
fn push_byte(output: &mut alloc::vec::Vec<u8>, el: u8) {
    output.push(el);
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_byte(output: &mut alloc::vec::Vec<u8>, el: u8) {
    unsafe {
        core::ptr::write(output.as_mut_ptr().add(output.len()), el);
        output.set_len(output.len() + 1);
    }
}