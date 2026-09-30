//! Deterministic fixtures for texscan tests: binary blobs with textures at known offsets,
//! plus traps that must not be reported. Everything comes from fixed seeds, so every run
//! produces the same bytes.
//!
//! `cargo run -p texscan-fixtures --bin gen-fixtures` writes them to `tests/fixtures/`.
//!
//! DDS and KTX2 headers are written here and sizes worked out here, independently of
//! texscan-core, so the tests compare two implementations.

/// xorshift64*: small, fast, and stable across platforms and releases.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n + 8);
        while out.len() < n {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(n);
        out
    }
}

/// How a DDS header describes its pixel format.
#[derive(Debug, Clone, Copy)]
pub enum Pf {
    /// Legacy FourCC, such as `DXT1`.
    FourCC([u8; 4]),
    /// Legacy uncompressed: pixel format flags, bit count and R, G, B, A masks.
    Masks { flags: u32, bits: u32, masks: [u32; 4] },
    /// DX10 header with this DXGI format.
    Dx10(u32),
}

/// How much space pixels take, for the independent size calculation.
#[derive(Debug, Clone, Copy)]
pub enum Unit {
    /// 4×4 blocks of this many bytes.
    Block(u64),
    /// Bits per pixel.
    Bits(u64),
    /// Blocks of width × height pixels, this many bytes each (ASTC).
    Tiles(u64, u64, u64),
}

#[derive(Debug, Clone)]
pub struct DdsSpec {
    pub width: u32,
    pub height: u32,
    /// 1 unless a volume texture.
    pub depth: u32,
    /// As written in the header: 0 means 1.
    pub mips: u32,
    pub array_size: u32,
    pub cube: bool,
    pub pf: Pf,
    pub unit: Unit,
    /// The name texscan should report.
    pub name: &'static str,
    pub dxgi: Option<u32>,
}

impl DdsSpec {
    pub fn new(width: u32, height: u32, mips: u32, pf: Pf, unit: Unit, name: &'static str, dxgi: Option<u32>) -> Self {
        Self { width, height, depth: 1, mips, array_size: 1, cube: false, pf, unit, name, dxgi }
    }

    pub fn header_size(&self) -> usize {
        if matches!(self.pf, Pf::Dx10(_)) { 148 } else { 128 }
    }

    pub fn faces(&self) -> u32 {
        if self.cube { 6 } else { 1 }
    }

    /// Bytes in one image of mip `i`.
    pub fn image_size(&self, i: u32) -> u64 {
        let w = u64::from((self.width >> i).max(1));
        let h = u64::from((self.height >> i).max(1));
        match self.unit {
            Unit::Block(bytes) => w.div_ceil(4) * h.div_ceil(4) * bytes,
            Unit::Bits(bits) => (w * bits).div_ceil(8) * h,
            Unit::Tiles(tw, th, bytes) => w.div_ceil(tw) * h.div_ceil(th) * bytes,
        }
    }

    /// Bytes in all of mip `i`: every layer, face and slice.
    pub fn level_size(&self, i: u32) -> usize {
        let d = u64::from((self.depth >> i).max(1));
        (self.image_size(i) * d * u64::from(self.faces() * self.array_size)) as usize
    }

    pub fn data_size(&self) -> usize {
        (0..self.mips.max(1)).map(|i| self.level_size(i)).sum()
    }

    pub fn header(&self) -> Vec<u8> {
        let mut h = vec![0u8; self.header_size()];
        let put = |h: &mut Vec<u8>, pos: usize, v: u32| h[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
        h[..4].copy_from_slice(b"DDS ");
        put(&mut h, 4, 124);
        let mut flags = 0x1007; // caps, height, width, pixel format
        if self.mips > 0 {
            flags |= 0x2_0000;
        }
        if self.depth > 1 {
            flags |= 0x80_0000;
        }
        put(&mut h, 8, flags);
        put(&mut h, 12, self.height);
        put(&mut h, 16, self.width);
        put(&mut h, 24, if self.depth > 1 { self.depth } else { 0 });
        put(&mut h, 28, self.mips);
        put(&mut h, 76, 32);
        match self.pf {
            Pf::FourCC(code) => {
                put(&mut h, 80, 0x4);
                h[84..88].copy_from_slice(&code);
            }
            Pf::Masks { flags, bits, masks } => {
                put(&mut h, 80, flags);
                put(&mut h, 88, bits);
                for (i, m) in masks.iter().enumerate() {
                    put(&mut h, 92 + 4 * i, *m);
                }
            }
            Pf::Dx10(format) => {
                put(&mut h, 80, 0x4);
                h[84..88].copy_from_slice(b"DX10");
                put(&mut h, 128, format);
                put(&mut h, 132, if self.depth > 1 { 4 } else { 3 });
                put(&mut h, 136, if self.cube { 0x4 } else { 0 });
                put(&mut h, 140, self.array_size);
            }
        }
        let mut caps = 0x1000;
        if self.mips > 1 || self.cube || self.depth > 1 {
            caps |= 0x8;
        }
        if self.mips > 1 {
            caps |= 0x40_0000;
        }
        put(&mut h, 108, caps);
        if !matches!(self.pf, Pf::Dx10(_)) {
            if self.cube {
                put(&mut h, 112, 0x200 | 0xFC00);
            } else if self.depth > 1 {
                put(&mut h, 112, 0x20_0000);
            }
        }
        h
    }

    /// Header plus random pixel data.
    pub fn build(&self, rng: &mut Rng) -> Vec<u8> {
        let mut out = self.header();
        out.extend(rng.bytes(self.data_size()));
        out
    }
}

/// A texture the scanner is expected to report.
#[derive(Debug, Clone)]
pub struct Expected {
    pub offset: usize,
    /// `dds` or `ktx2`.
    pub container: &'static str,
    /// The texture's shape and format (for KTX2, `pf` is unused).
    pub spec: DdsSpec,
    pub size: usize,
    pub crc32: u32,
    /// Whether texscan can decode it (not ASTC, ETC or supercompressed).
    pub decodable: bool,
}

/// A header the scanner should report as unusable, with how its reason starts.
#[derive(Debug, Clone)]
pub struct ExpectedReject {
    pub offset: usize,
    pub reason_prefix: &'static str,
}

#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: &'static str,
    pub description: &'static str,
    pub data: Vec<u8>,
    pub expected: Vec<Expected>,
    pub rejected: Vec<ExpectedReject>,
}

impl Fixture {
    fn new(name: &'static str, description: &'static str) -> Self {
        Self { name, description, data: Vec::new(), expected: Vec::new(), rejected: Vec::new() }
    }

    fn texture(&mut self, spec: DdsSpec, rng: &mut Rng) {
        let bytes = spec.build(rng);
        self.push("dds", spec, bytes, true);
    }

    fn push(&mut self, container: &'static str, spec: DdsSpec, bytes: Vec<u8>, decodable: bool) {
        let crc32 = crc32fast::hash(&bytes);
        self.expected.push(Expected { offset: self.data.len(), container, size: bytes.len(), crc32, spec, decodable });
        self.data.extend(bytes);
    }

    fn filler(&mut self, n: usize, rng: &mut Rng) {
        self.data.extend(rng.bytes(n));
    }
}

const RGB: u32 = 0x40;
const ALPHAPIXELS: u32 = 0x1;
const LUMINANCE: u32 = 0x2_0000;

pub fn dxt(code: &[u8; 4]) -> (Pf, Unit) {
    let bytes = if matches!(code, b"DXT1" | b"ATI1" | b"BC4U" | b"BC4S") { 8 } else { 16 };
    (Pf::FourCC(*code), Unit::Block(bytes))
}

/// Textures of every kind of DDS layout inside a made-up archive, with traps.
pub fn dds_archive() -> Fixture {
    let mut rng = Rng::new(0xDD5);
    let mut f = Fixture::new(
        "dds_archive",
        "DDS textures (legacy, DX10, mips, cube maps, volume, arrays, odd sizes) in an archive, plus traps",
    );
    f.data.extend_from_slice(b"PAK1");
    f.filler(60, &mut rng);
    // Trap: the magic in text. The header size field reads "file", not 124.
    f.data.extend_from_slice(b"Textures are stored as DDS files in this archive.\n");

    // A DXT1 texture whose pixel data contains a complete, valid DDS: found textures are
    // skipped past, so the inner one must not be reported.
    let (pf, unit) = dxt(b"DXT1");
    let outer = DdsSpec::new(256, 128, 9, pf, unit, "DXT1", Some(71));
    let mut bytes = outer.build(&mut rng);
    let (pf, unit) = dxt(b"DXT5");
    let inner = DdsSpec::new(8, 8, 1, pf, unit, "DXT5", Some(77)).build(&mut rng);
    bytes[1000..1000 + inner.len()].copy_from_slice(&inner);
    f.push("dds", outer, bytes, true);
    f.filler(37, &mut rng);

    let (pf, unit) = dxt(b"DXT5");
    f.texture(DdsSpec::new(64, 64, 0, pf, unit, "DXT5", Some(77)), &mut rng);
    let argb = Pf::Masks { flags: RGB | ALPHAPIXELS, bits: 32, masks: [0xff_0000, 0xff00, 0xff, 0xff00_0000] };
    f.texture(DdsSpec::new(32, 16, 3, argb, Unit::Bits(32), "A8R8G8B8", Some(87)), &mut rng);
    f.filler(5, &mut rng);
    f.texture(DdsSpec::new(128, 128, 8, Pf::Dx10(99), Unit::Block(16), "BC7_UNORM_SRGB", Some(99)), &mut rng);
    let mut cube = DdsSpec::new(32, 32, 6, Pf::Dx10(71), Unit::Block(8), "BC1_UNORM", Some(71));
    cube.cube = true;
    f.texture(cube, &mut rng);
    let rgb565 = Pf::Masks { flags: RGB, bits: 16, masks: [0xf800, 0x7e0, 0x1f, 0] };
    let mut legacy_cube = DdsSpec::new(16, 16, 1, rgb565, Unit::Bits(16), "R5G6B5", Some(85));
    legacy_cube.cube = true;
    f.texture(legacy_cube, &mut rng);
    f.filler(11, &mut rng);
    let l8 = Pf::Masks { flags: LUMINANCE, bits: 8, masks: [0xff, 0, 0, 0] };
    let mut volume = DdsSpec::new(16, 16, 5, l8, Unit::Bits(8), "L8", Some(61));
    volume.depth = 8;
    f.texture(volume, &mut rng);
    f.texture(DdsSpec::new(40, 24, 6, Pf::Dx10(10), Unit::Bits(64), "R16G16B16A16_FLOAT", Some(10)), &mut rng);
    f.filler(64, &mut rng);
    // Odd sizes, then another texture straight after.
    let (pf, unit) = dxt(b"DXT5");
    f.texture(DdsSpec::new(100, 60, 7, pf, unit, "DXT5", Some(77)), &mut rng);
    let (pf, unit) = dxt(b"ATI2");
    f.texture(DdsSpec::new(8, 8, 4, pf, unit, "ATI2", Some(83)), &mut rng);
    let mut array = DdsSpec::new(16, 16, 5, Pf::Dx10(98), Unit::Block(16), "BC7_UNORM", Some(98));
    array.array_size = 3;
    f.texture(array, &mut rng);
    f.filler(20, &mut rng);

    // Trap: right header size, wrong pixel-format size. Not a DDS at all, so not reported.
    let (pf, unit) = dxt(b"DXT1");
    let mut not_dds = DdsSpec::new(16, 16, 1, pf, unit, "", None).build(&mut rng);
    not_dds[76] = 0;
    f.data.extend(not_dds);
    f.filler(9, &mut rng);
    // A DDS with a pixel format texscan doesn't know: reported as rejected.
    f.rejected.push(ExpectedReject { offset: f.data.len(), reason_prefix: "unknown FourCC" });
    f.data.extend(DdsSpec::new(16, 16, 1, Pf::FourCC(*b"ZZZZ"), Unit::Block(8), "", None).build(&mut rng));
    f.filler(33, &mut rng);
    // Cut off by the end of the file.
    f.rejected.push(ExpectedReject { offset: f.data.len(), reason_prefix: "truncated" });
    let (pf, unit) = dxt(b"DXT5");
    let mut cut = DdsSpec::new(256, 256, 1, pf, unit, "", None).build(&mut rng);
    cut.truncate(228);
    f.data.extend(cut);
    f
}

/// A plain `.dds` file: one texture at offset 0 and nothing else.
pub fn dds_file() -> Fixture {
    let mut rng = Rng::new(0xF11E);
    let mut f = Fixture::new("dds_file", "A single DX10 BC3 texture filling the whole file");
    f.texture(DdsSpec::new(64, 64, 7, Pf::Dx10(77), Unit::Block(16), "BC3_UNORM", Some(77)), &mut rng);
    f
}

pub const KTX2_MAGIC: &[u8; 12] = b"\xABKTX 20\xBB\r\n\x1A\n";

/// A KTX2 texture of `spec`'s shape with Vulkan format `vk`: header, level index, a
/// small data format descriptor and key/value data, then the levels smallest first,
/// each 16-byte aligned. With `scheme` (supercompression) non-zero the levels get
/// made-up lengths, as compressed data would. `spec.mips` 0 is written as a level
/// count of 0 (one level).
pub fn ktx2(spec: &DdsSpec, vk: u32, scheme: u32, rng: &mut Rng) -> Vec<u8> {
    let levels = spec.mips.max(1) as usize;
    let index_at = 80;
    let dfd_at = index_at + levels * 24;
    let dfd_len = 44;
    let kvd_at = dfd_at + dfd_len;
    let mut kvd = Vec::new();
    let entry = b"KTXwriter\0texscan fixtures\0";
    kvd.extend_from_slice(&(entry.len() as u32).to_le_bytes());
    kvd.extend_from_slice(entry);
    kvd.resize(kvd.len().next_multiple_of(4), 0);
    let layers = if spec.array_size > 1 { spec.array_size } else { 0 };
    let depth = if spec.depth > 1 { spec.depth } else { 0 };
    let mut out = KTX2_MAGIC.to_vec();
    for v in [vk, 1, spec.width, spec.height, depth, layers, spec.faces(), spec.mips, scheme] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in [dfd_at as u32, dfd_len as u32, kvd_at as u32, kvd.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[0u8; 16]);
    // Level data, smallest mip first.
    let mut at = kvd_at + kvd.len();
    let mut placed = vec![(0usize, Vec::new()); levels];
    for m in (0..levels).rev() {
        at = at.next_multiple_of(16);
        let len = if scheme == 0 { spec.level_size(m as u32) } else { spec.level_size(m as u32) / 3 + 5 };
        placed[m] = (at, rng.bytes(len));
        at += len;
    }
    for (offset, bytes) in &placed {
        let uncompressed = bytes.len() as u64;
        for v in [*offset as u64, bytes.len() as u64, uncompressed] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(&(dfd_len as u32).to_le_bytes());
    out.resize(kvd_at, 0);
    out.extend_from_slice(&kvd);
    out.resize(at, 0);
    for (offset, bytes) in placed {
        out[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }
    out
}

/// KTX2 textures of every layout (and a DDS among them) in an archive, plus traps.
pub fn ktx2_archive() -> Fixture {
    let mut rng = Rng::new(0x4B7);
    let mut f = Fixture::new(
        "ktx2_archive",
        "KTX2 textures (BC7, cube, array, volume, 24-bit, ASTC, supercompressed) and a DDS in an archive, plus traps",
    );
    f.data.extend_from_slice(b"KPAK");
    f.filler(44, &mut rng);
    let add = |f: &mut Fixture, rng: &mut Rng, spec: DdsSpec, vk: u32, scheme: u32, decodable: bool| {
        let bytes = ktx2(&spec, vk, scheme, rng);
        f.push("ktx2", spec, bytes, decodable);
    };
    let unit16 = Unit::Block(16);
    add(&mut f, &mut rng, DdsSpec::new(128, 64, 8, Pf::Dx10(0), unit16, "BC7_SRGB_BLOCK", Some(99)), 146, 0, true);
    f.filler(13, &mut rng);
    let mut cube = DdsSpec::new(32, 32, 6, Pf::Dx10(0), Unit::Bits(32), "R8G8B8A8_SRGB", Some(29));
    cube.cube = true;
    add(&mut f, &mut rng, cube, 43, 0, true);
    let mut array = DdsSpec::new(16, 16, 5, Pf::Dx10(0), Unit::Bits(64), "R16G16B16A16_SFLOAT", Some(10));
    array.array_size = 3;
    add(&mut f, &mut rng, array, 97, 0, true);
    f.filler(7, &mut rng);
    add(&mut f, &mut rng, DdsSpec::new(20, 12, 5, Pf::Dx10(0), Unit::Bits(24), "R8G8B8_UNORM", None), 23, 0, true);
    add(&mut f, &mut rng, DdsSpec::new(16, 8, 0, Pf::Dx10(0), Unit::Bits(8), "R8_UNORM", Some(61)), 9, 0, true);
    let mut volume = DdsSpec::new(16, 16, 5, Pf::Dx10(0), Unit::Bits(32), "B8G8R8A8_UNORM", Some(87));
    volume.depth = 4;
    add(&mut f, &mut rng, volume, 44, 0, true);
    f.filler(29, &mut rng);
    // A DDS among them.
    let (pf, unit) = dxt(b"DXT1");
    f.texture(DdsSpec::new(32, 32, 6, pf, unit, "DXT1", Some(71)), &mut rng);
    let astc = DdsSpec::new(50, 30, 1, Pf::Dx10(0), Unit::Tiles(6, 6, 16), "ASTC_6x6_UNORM_BLOCK", None);
    add(&mut f, &mut rng, astc, 165, 0, false);
    let zstd = DdsSpec::new(64, 64, 7, Pf::Dx10(0), Unit::Block(8), "BC1_RGBA_UNORM_BLOCK", Some(71));
    add(&mut f, &mut rng, zstd, 133, 2, false);
    f.filler(40, &mut rng);

    // Three faces: clearly KTX2, but unusable.
    f.rejected.push(ExpectedReject { offset: f.data.len(), reason_prefix: "3 faces" });
    let mut bad = ktx2(&DdsSpec::new(8, 8, 1, Pf::Dx10(0), Unit::Bits(32), "", None), 37, 0, &mut rng);
    bad[36..40].copy_from_slice(&3u32.to_le_bytes());
    f.data.extend(bad);
    f.filler(21, &mut rng);
    // Cut off by the end of the file.
    f.rejected.push(ExpectedReject { offset: f.data.len(), reason_prefix: "truncated" });
    let mut cut = ktx2(&DdsSpec::new(64, 64, 1, Pf::Dx10(0), Unit::Bits(32), "", None), 37, 0, &mut rng);
    cut.truncate(cut.len() - 100);
    f.data.extend(cut);
    f
}

pub fn all() -> Vec<Fixture> {
    vec![dds_archive(), dds_file(), ktx2_archive()]
}
