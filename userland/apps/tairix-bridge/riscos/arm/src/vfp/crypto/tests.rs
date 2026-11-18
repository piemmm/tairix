use super::*;

fn bytes(value: u128) -> [u8; 16] {
    value.to_le_bytes()
}

#[test]
fn the_s_box_is_fips_197_s() {
    let sbox = |x: u8| bytes(sub_bytes(u128::from(x)))[0];
    let inverse = |x: u8| bytes(inv_sub_bytes(u128::from(x)))[0];
    assert_eq!(sbox(0x00), 0x63);
    assert_eq!(sbox(0x01), 0x7C);
    assert_eq!(sbox(0x53), 0xED);
    assert_eq!(sbox(0xFF), 0x16);
    for x in 0..=255u8 {
        assert_eq!(inverse(sbox(x)), x, "{x:02X}");
    }
    // Every byte of a state at once, each its own.
    let state = u128::from_le_bytes(core::array::from_fn(|i| u8::try_from(i * 17).unwrap()));
    let each: [u8; 16] = core::array::from_fn(|i| sbox(bytes(state)[i]));
    assert_eq!(bytes(sub_bytes(state)), each);
}

/// AES-128's round keys, as FIPS 197 expands them.
fn round_keys(key: [u8; 16]) -> [u128; 11] {
    let mut w = [[0u8; 4]; 44];
    for (i, word) in w.iter_mut().take(4).enumerate() {
        word.copy_from_slice(&key[4 * i..4 * i + 4]);
    }
    let mut rcon = 1u8;
    for i in 4..44 {
        let mut temp = w[i - 1];
        if i % 4 == 0 {
            temp.rotate_left(1);
            let substituted = bytes(sub_bytes(u128::from(u32::from_le_bytes(temp))));
            temp.copy_from_slice(&substituted[..4]);
            temp[0] ^= rcon;
            rcon = bytes(double(u128::from(rcon)))[0];
        }
        for j in 0..4 {
            w[i][j] = w[i - 4][j] ^ temp[j];
        }
    }
    core::array::from_fn(|round| {
        let mut block = [0u8; 16];
        for c in 0..4 {
            block[4 * c..4 * c + 4].copy_from_slice(&w[4 * round + c]);
        }
        u128::from_le_bytes(block)
    })
}

fn block(hex: &str) -> u128 {
    let bytes: [u8; 16] =
        core::array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap());
    u128::from_le_bytes(bytes)
}

#[test]
fn aes_128_encrypts_and_decrypts_fips_197_s_example() {
    let keys = round_keys(bytes(block("000102030405060708090a0b0c0d0e0f")));
    let plaintext = block("00112233445566778899aabbccddeeff");
    let ciphertext = block("69c4e0d86a7b0430d8cdb78070b4c55a");
    let mut state = plaintext;
    for key in &keys[..9] {
        state = aesmc(aese(state, *key));
    }
    state = aese(state, keys[9]) ^ keys[10];
    assert_eq!(state, ciphertext);
    let mut state = aesd(ciphertext, keys[10]);
    for key in keys[1..10].iter().rev() {
        state = aesd(aesimc(state ^ key), 0);
    }
    assert_eq!(state ^ keys[0], plaintext);
}

#[test]
fn mix_columns_and_its_inverse_undo_each_other() {
    let state = block("db135345f20a225c01010101c6c6c6c6");
    assert_eq!(aesimc(aesmc(state)), state);
    // FIPS 197's worked column: db 13 53 45 mixes to 8e 4d a1 bc.
    assert_eq!(bytes(aesmc(state))[..4], [0x8E, 0x4D, 0xA1, 0xBC]);
}

/// "abc" padded to one 512-bit block, as big-endian words.
fn abc_block() -> [u32; 16] {
    let mut message = [0u8; 64];
    message[..3].copy_from_slice(b"abc");
    message[3] = 0x80;
    message[63] = 24;
    core::array::from_fn(|i| u32::from_be_bytes(message[4 * i..4 * i + 4].try_into().unwrap()))
}

fn quads(words_: &[u32; 16]) -> [u128; 4] {
    core::array::from_fn(|i| {
        words([
            words_[4 * i],
            words_[4 * i + 1],
            words_[4 * i + 2],
            words_[4 * i + 3],
        ])
    })
}

fn lanewise_add(a: u128, b: u128) -> u128 {
    words(core::array::from_fn(|e| {
        word(a, u32::try_from(e).unwrap()).wrapping_add(word(b, u32::try_from(e).unwrap()))
    }))
}

#[test]
fn sha_1_hashes_abc_through_its_instructions() {
    let k = [0x5A82_7999u32, 0x6ED9_EBA1, 0x8F1B_BCDC, 0xCA62_C1D6];
    let h = [
        0x6745_2301u32,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut q = quads(&abc_block());
    let mut abcd = words([h[0], h[1], h[2], h[3]]);
    let mut e = u128::from(h[4]);
    for group in 0..20 {
        let wk = lanewise_add(q[group % 4], words([k[group / 5]; 4]));
        let function = match group / 5 {
            0 => Sha1::Choose,
            2 => Sha1::Majority,
            _ => Sha1::Parity,
        };
        let next_e = sha1h(abcd);
        abcd = sha1_hash(function, abcd, e, wk);
        e = next_e;
        if group < 16 {
            let partial = sha1su0(q[group % 4], q[(group + 1) % 4], q[(group + 2) % 4]);
            q[group % 4] = sha1su1(partial, q[(group + 3) % 4]);
        }
    }
    let digest = [
        h[0].wrapping_add(word(abcd, 0)),
        h[1].wrapping_add(word(abcd, 1)),
        h[2].wrapping_add(word(abcd, 2)),
        h[3].wrapping_add(word(abcd, 3)),
        h[4].wrapping_add(word(e, 0)),
    ];
    assert_eq!(
        digest,
        [
            0xA999_3E36,
            0x4706_816A,
            0xBA3E_2571,
            0x7850_C26C,
            0x9CD0_D89D
        ]
    );
}

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

#[test]
fn sha_256_hashes_abc_through_its_instructions() {
    let h = [
        0x6a09_e667u32,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut q = quads(&abc_block());
    let mut abcd = words([h[0], h[1], h[2], h[3]]);
    let mut efgh = words([h[4], h[5], h[6], h[7]]);
    let constants: [u128; 16] =
        core::array::from_fn(|group| words(core::array::from_fn(|e| SHA256_K[4 * group + e])));
    for (group, constant) in constants.iter().enumerate() {
        let wk = lanewise_add(q[group % 4], *constant);
        let previous = abcd;
        abcd = sha256h(abcd, efgh, wk);
        efgh = sha256h2(efgh, previous, wk);
        if group < 12 {
            let partial = sha256su0(q[group % 4], q[(group + 1) % 4]);
            q[group % 4] = sha256su1(partial, q[(group + 2) % 4], q[(group + 3) % 4]);
        }
    }
    let digest: [u32; 8] = core::array::from_fn(|i| {
        let lane = u32::try_from(i % 4).unwrap();
        h[i].wrapping_add(word(if i < 4 { abcd } else { efgh }, lane))
    });
    assert_eq!(
        digest,
        [
            0xba78_16bf,
            0x8f01_cfea,
            0x4141_40de,
            0x5dae_2223,
            0xb003_61a3,
            0x9617_7a9c,
            0xb410_ff61,
            0xf200_15ad
        ]
    );
}
