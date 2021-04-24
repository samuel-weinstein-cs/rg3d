// Keep this for now, some texture kind might be used in future.
#![allow(dead_code)]

use crate::{
    core::color::Color,
    renderer::{error::RendererError, framework::state::PipelineState},
    resource::texture::{
        TextureKind, TextureMagnificationFilter, TextureMinificationFilter, TexturePixelKind,
        TextureWrapMode,
    },
    utils::log::{Log, MessageKind},
};
use glow::{HasContext, COMPRESSED_RED_RGTC1, COMPRESSED_RG_RGTC2};
use std::marker::PhantomData;

#[derive(Copy, Clone)]
pub enum GpuTextureKind {
    Line {
        length: usize,
    },
    Rectangle {
        width: usize,
        height: usize,
    },
    Cube {
        width: usize,
        height: usize,
    },
    Volume {
        width: usize,
        height: usize,
        depth: usize,
    },
}

impl From<TextureKind> for GpuTextureKind {
    fn from(v: TextureKind) -> Self {
        match v {
            TextureKind::Line { length } => GpuTextureKind::Line {
                length: length as usize,
            },
            TextureKind::Rectangle { width, height } => GpuTextureKind::Rectangle {
                width: width as usize,
                height: height as usize,
            },
            TextureKind::Cube { width, height } => GpuTextureKind::Cube {
                width: width as usize,
                height: height as usize,
            },
            TextureKind::Volume {
                width,
                height,
                depth,
            } => GpuTextureKind::Volume {
                width: width as usize,
                height: height as usize,
                depth: depth as usize,
            },
        }
    }
}

impl GpuTextureKind {
    fn to_texture_target(&self) -> u32 {
        match self {
            Self::Line { .. } => glow::TEXTURE_1D,
            Self::Rectangle { .. } => glow::TEXTURE_2D,
            Self::Cube { .. } => glow::TEXTURE_CUBE_MAP,
            Self::Volume { .. } => glow::TEXTURE_3D,
        }
    }
}

#[derive(Copy, Clone)]
pub enum PixelKind {
    F32,
    F16,
    D32,
    D16,
    D24S8,
    RGBA8,
    RGB8,
    BGRA8,
    BGR8,
    RG8,
    RG16,
    R8,
    R16,
    RGB16,
    RGBA16,
    DXT1RGB,
    DXT1RGBA,
    DXT3RGBA,
    DXT5RGBA,
    RGBA32F,
    R8RGTC,
    RG8RGTC,
}

impl From<TexturePixelKind> for PixelKind {
    fn from(texture_kind: TexturePixelKind) -> Self {
        match texture_kind {
            TexturePixelKind::R8 => Self::R8,
            TexturePixelKind::RGB8 => Self::RGB8,
            TexturePixelKind::RGBA8 => Self::RGBA8,
            TexturePixelKind::RG8 => Self::RG8,
            TexturePixelKind::R16 => Self::R16,
            TexturePixelKind::RG16 => Self::RG16,
            TexturePixelKind::BGR8 => Self::BGR8,
            TexturePixelKind::BGRA8 => Self::BGRA8,
            TexturePixelKind::RGB16 => Self::RGB16,
            TexturePixelKind::RGBA16 => Self::RGBA16,
            TexturePixelKind::DXT1RGB => Self::DXT1RGB,
            TexturePixelKind::DXT1RGBA => Self::DXT1RGBA,
            TexturePixelKind::DXT3RGBA => Self::DXT3RGBA,
            TexturePixelKind::DXT5RGBA => Self::DXT5RGBA,
            TexturePixelKind::R8RGTC => Self::R8RGTC,
            TexturePixelKind::RG8RGTC => Self::RG8RGTC,
        }
    }
}

impl PixelKind {
    fn unpack_alignment(self) -> i32 {
        match self {
            Self::RGBA16 | Self::RGB16 | Self::RGBA32F => 8,
            Self::RGBA8
            | Self::RGB8
            | Self::BGRA8
            | Self::BGR8
            | Self::RG16
            | Self::R16
            | Self::D24S8
            | Self::D32
            | Self::F32 => 4,
            Self::RG8 | Self::D16 | Self::F16 => 2,
            Self::R8 => 1,
            Self::DXT1RGB
            | Self::DXT1RGBA
            | Self::DXT3RGBA
            | Self::DXT5RGBA
            | Self::R8RGTC
            | Self::RG8RGTC => unreachable!(),
        }
    }

    fn is_compressed(self) -> bool {
        match self {
            Self::DXT1RGB
            | Self::DXT1RGBA
            | Self::DXT3RGBA
            | Self::DXT5RGBA
            | Self::R8RGTC
            | Self::RG8RGTC => true,
            // Explicit match for rest of formats instead of _ will help to not forget
            // to add new entry here.
            Self::RGBA16
            | Self::RGB16
            | Self::RGBA8
            | Self::RGB8
            | Self::BGRA8
            | Self::BGR8
            | Self::RG16
            | Self::R16
            | Self::D24S8
            | Self::D32
            | Self::F32
            | Self::RG8
            | Self::D16
            | Self::F16
            | Self::R8
            | Self::RGBA32F => false,
        }
    }
}

pub struct GpuTexture {
    state: *mut PipelineState,
    texture: glow::Texture,
    kind: GpuTextureKind,
    min_filter: MinificationFilter,
    mag_filter: MagnificationFilter,
    s_wrap_mode: WrapMode,
    t_wrap_mode: WrapMode,
    r_wrap_mode: WrapMode,
    anisotropy: f32,
    pixel_kind: PixelKind,
    // Force compiler to not implement Send and Sync, because OpenGL is not thread-safe.
    thread_mark: PhantomData<*const u8>,
}

fn ceil_div_4(x: usize) -> usize {
    (x + 3) / 4
}

fn image_3d_size_bytes(pixel_kind: PixelKind, width: usize, height: usize, depth: usize) -> usize {
    let pixel_count = width * height * depth;
    match pixel_kind {
        PixelKind::RGBA32F => 16 * pixel_count,
        PixelKind::RGBA16 => 8 * pixel_count,
        PixelKind::RGB16 => 6 * pixel_count,
        PixelKind::RGBA8
        | PixelKind::BGRA8
        | PixelKind::RG16
        | PixelKind::D24S8
        | PixelKind::D32
        | PixelKind::F32 => 4 * pixel_count,
        PixelKind::RGB8 | PixelKind::BGR8 => 3 * pixel_count,
        PixelKind::RG8 | PixelKind::R16 | PixelKind::D16 | PixelKind::F16 => 2 * pixel_count,
        PixelKind::R8 => pixel_count,
        PixelKind::DXT1RGB | PixelKind::DXT1RGBA | PixelKind::R8RGTC => {
            let block_size = 8;
            ceil_div_4(width) * ceil_div_4(height) * ceil_div_4(depth) * block_size
        }
        PixelKind::DXT3RGBA | PixelKind::DXT5RGBA | PixelKind::RG8RGTC => {
            let block_size = 16;
            ceil_div_4(width) * ceil_div_4(height) * ceil_div_4(depth) * block_size
        }
    }
}

fn image_2d_size_bytes(pixel_kind: PixelKind, width: usize, height: usize) -> usize {
    let pixel_count = width * height;
    match pixel_kind {
        PixelKind::RGBA32F => 16 * pixel_count,
        PixelKind::RGBA16 => 8 * pixel_count,
        PixelKind::RGB16 => 6 * pixel_count,
        PixelKind::RGBA8
        | PixelKind::BGRA8
        | PixelKind::RG16
        | PixelKind::D24S8
        | PixelKind::D32
        | PixelKind::F32 => 4 * pixel_count,
        PixelKind::RGB8 | PixelKind::BGR8 => 3 * pixel_count,
        PixelKind::RG8 | PixelKind::R16 | PixelKind::D16 | PixelKind::F16 => 2 * pixel_count,
        PixelKind::R8 => pixel_count,
        PixelKind::DXT1RGB | PixelKind::DXT1RGBA | PixelKind::R8RGTC => {
            let block_size = 8;
            ceil_div_4(width) * ceil_div_4(height) * block_size
        }
        PixelKind::DXT3RGBA | PixelKind::DXT5RGBA | PixelKind::RG8RGTC => {
            let block_size = 16;
            ceil_div_4(width) * ceil_div_4(height) * block_size
        }
    }
}

fn image_1d_size_bytes(pixel_kind: PixelKind, length: usize) -> usize {
    match pixel_kind {
        PixelKind::RGBA32F => 16 * length,
        PixelKind::RGBA16 => 8 * length,
        PixelKind::RGB16 => 6 * length,
        PixelKind::RGBA8
        | PixelKind::BGRA8
        | PixelKind::RG16
        | PixelKind::D24S8
        | PixelKind::D32
        | PixelKind::F32 => 4 * length,
        PixelKind::RGB8 | PixelKind::BGR8 => 3 * length,
        PixelKind::RG8 | PixelKind::R16 | PixelKind::D16 | PixelKind::F16 => 2 * length,
        PixelKind::R8 => length,
        PixelKind::DXT1RGB | PixelKind::DXT1RGBA | PixelKind::R8RGTC => {
            let block_size = 8;
            ceil_div_4(length) * block_size
        }
        PixelKind::DXT3RGBA | PixelKind::DXT5RGBA | PixelKind::RG8RGTC => {
            let block_size = 16;
            ceil_div_4(length) * block_size
        }
    }
}

#[derive(Copy, Clone, PartialOrd, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MagnificationFilter {
    Nearest,
    Linear,
}

impl MagnificationFilter {
    pub fn into_gl_value(self) -> i32 {
        (match self {
            Self::Nearest => glow::NEAREST,
            Self::Linear => glow::LINEAR,
        }) as i32
    }
}

impl From<TextureMagnificationFilter> for MagnificationFilter {
    fn from(v: TextureMagnificationFilter) -> Self {
        match v {
            TextureMagnificationFilter::Nearest => Self::Nearest,
            TextureMagnificationFilter::Linear => Self::Linear,
        }
    }
}

#[derive(Copy, Clone, PartialOrd, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MinificationFilter {
    Nearest = glow::NEAREST,
    NearestMipMapNearest = glow::NEAREST_MIPMAP_NEAREST,
    NearestMipMapLinear = glow::NEAREST_MIPMAP_LINEAR,
    Linear = glow::LINEAR,
    LinearMipMapNearest = glow::LINEAR_MIPMAP_NEAREST,
    LinearMipMapLinear = glow::LINEAR_MIPMAP_LINEAR,
}

impl From<TextureMinificationFilter> for MinificationFilter {
    fn from(v: TextureMinificationFilter) -> Self {
        match v {
            TextureMinificationFilter::Nearest => Self::Nearest,
            TextureMinificationFilter::NearestMipMapNearest => Self::NearestMipMapNearest,
            TextureMinificationFilter::NearestMipMapLinear => Self::NearestMipMapLinear,
            TextureMinificationFilter::Linear => Self::Linear,
            TextureMinificationFilter::LinearMipMapNearest => Self::LinearMipMapNearest,
            TextureMinificationFilter::LinearMipMapLinear => Self::LinearMipMapLinear,
        }
    }
}
impl MinificationFilter {
    pub fn into_gl_value(self) -> i32 {
        self as i32
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
pub enum WrapMode {
    Repeat = glow::REPEAT,
    ClampToEdge = glow::CLAMP_TO_EDGE,
    ClampToBorder = glow::CLAMP_TO_BORDER,
    MirroredRepeat = glow::MIRRORED_REPEAT,
    MirrorClampToEdge = glow::MIRROR_CLAMP_TO_EDGE,
}

impl WrapMode {
    pub fn into_gl_value(self) -> i32 {
        self as i32
    }
}

impl From<TextureWrapMode> for WrapMode {
    fn from(v: TextureWrapMode) -> Self {
        match v {
            TextureWrapMode::Repeat => WrapMode::Repeat,
            TextureWrapMode::ClampToEdge => WrapMode::ClampToEdge,
            TextureWrapMode::ClampToBorder => WrapMode::ClampToBorder,
            TextureWrapMode::MirroredRepeat => WrapMode::MirroredRepeat,
            TextureWrapMode::MirrorClampToEdge => WrapMode::MirrorClampToEdge,
        }
    }
}

#[derive(Copy, Clone)]
#[repr(u32)]
pub enum Coordinate {
    S = glow::TEXTURE_WRAP_S,
    T = glow::TEXTURE_WRAP_T,
    R = glow::TEXTURE_WRAP_R,
}

impl Coordinate {
    pub fn into_gl_value(self) -> u32 {
        self as u32
    }
}

pub struct TextureBinding<'a> {
    state: &'a mut PipelineState,
    texture: &'a mut GpuTexture,
}

#[derive(Copy, Clone)]
pub enum CubeMapFace {
    PositiveX,
    NegativeX,
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

impl CubeMapFace {
    pub fn into_gl_value(self) -> u32 {
        match self {
            Self::PositiveX => glow::TEXTURE_CUBE_MAP_POSITIVE_X,
            Self::NegativeX => glow::TEXTURE_CUBE_MAP_NEGATIVE_X,
            Self::PositiveY => glow::TEXTURE_CUBE_MAP_POSITIVE_Y,
            Self::NegativeY => glow::TEXTURE_CUBE_MAP_NEGATIVE_Y,
            Self::PositiveZ => glow::TEXTURE_CUBE_MAP_POSITIVE_Z,
            Self::NegativeZ => glow::TEXTURE_CUBE_MAP_NEGATIVE_Z,
        }
    }
}

impl<'a> TextureBinding<'a> {
    pub fn set_anisotropy(self, anisotropy: f32) -> Self {
        unsafe {
            let max = self
                .state
                .gl
                .get_parameter_f32(glow::MAX_TEXTURE_MAX_ANISOTROPY_EXT);
            self.state.gl.tex_parameter_f32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAX_ANISOTROPY_EXT,
                anisotropy.max(1.0).min(max),
            );

            // Set it to requested value, instead of hardware-limited. This will allow
            // us to check if anisotropy needs to be changed.
            self.texture.anisotropy = anisotropy;
        }
        self
    }

    pub fn set_minification_filter(self, min_filter: MinificationFilter) -> Self {
        unsafe {
            let target = self.texture.kind.to_texture_target();

            self.state.gl.tex_parameter_i32(
                target,
                glow::TEXTURE_MIN_FILTER,
                min_filter.into_gl_value(),
            );

            if self.texture.min_filter != MinificationFilter::Linear
                && self.texture.min_filter != MinificationFilter::Nearest
            {
                self.state.gl.generate_mipmap(target);
            }

            self.texture.min_filter = min_filter;
        }
        self
    }

    pub fn set_magnification_filter(self, mag_filter: MagnificationFilter) -> Self {
        unsafe {
            self.state.gl.tex_parameter_i32(
                self.texture.kind.to_texture_target(),
                glow::TEXTURE_MAG_FILTER,
                mag_filter.into_gl_value(),
            );

            self.texture.mag_filter = mag_filter;
        }
        self
    }

    pub fn set_wrap(self, coordinate: Coordinate, wrap: WrapMode) -> Self {
        unsafe {
            self.state.gl.tex_parameter_i32(
                self.texture.kind.to_texture_target(),
                coordinate.into_gl_value(),
                wrap.into_gl_value(),
            );

            match coordinate {
                Coordinate::S => self.texture.s_wrap_mode = wrap,
                Coordinate::T => self.texture.t_wrap_mode = wrap,
                Coordinate::R => self.texture.r_wrap_mode = wrap,
            }
        }
        self
    }

    pub fn set_border_color(self, color: Color) -> Self {
        unsafe {
            let color = color.as_frgba();
            let color = [color.x, color.y, color.z, color.w];
            self.state.gl.tex_parameter_f32_slice(
                self.texture.kind.to_texture_target(),
                glow::TEXTURE_BORDER_COLOR,
                &color,
            );
        }
        self
    }

    pub fn generate_mip_maps(self) -> Self {
        unsafe {
            self.state
                .gl
                .generate_mipmap(self.texture.kind.to_texture_target());
        }
        self
    }

    pub fn set_data(
        self,
        kind: GpuTextureKind,
        pixel_kind: PixelKind,
        mip_count: usize,
        data: Option<&[u8]>,
    ) -> Result<Self, RendererError> {
        let mip_count = mip_count.max(1);

        let mut desired_byte_count = 0;

        'mip_loop: for mip in 0..mip_count {
            match kind {
                GpuTextureKind::Line { length } => {
                    if let Some(length) = length.checked_shr(mip as u32) {
                        desired_byte_count += image_1d_size_bytes(pixel_kind, length);
                    } else {
                        break 'mip_loop;
                    }
                }
                GpuTextureKind::Rectangle { width, height } => {
                    if let (Some(width), Some(height)) = (
                        width.checked_shr(mip as u32),
                        height.checked_shr(mip as u32),
                    ) {
                        desired_byte_count += image_2d_size_bytes(pixel_kind, width, height);
                    } else {
                        break 'mip_loop;
                    }
                }
                GpuTextureKind::Cube { width, height } => {
                    if let (Some(width), Some(height)) = (
                        width.checked_shr(mip as u32),
                        height.checked_shr(mip as u32),
                    ) {
                        desired_byte_count += 6 * image_2d_size_bytes(pixel_kind, width, height);
                    } else {
                        break 'mip_loop;
                    }
                }
                GpuTextureKind::Volume {
                    width,
                    height,
                    depth,
                } => {
                    if let (Some(width), Some(height), Some(depth)) = (
                        width.checked_shr(mip as u32),
                        height.checked_shr(mip as u32),
                        depth.checked_shr(mip as u32),
                    ) {
                        desired_byte_count += image_3d_size_bytes(pixel_kind, width, height, depth);
                    } else {
                        break 'mip_loop;
                    }
                }
            };
        }

        if let Some(data) = data {
            let actual_data_size = data.len();
            if actual_data_size != desired_byte_count {
                return Err(RendererError::InvalidTextureData {
                    expected_data_size: desired_byte_count,
                    actual_data_size,
                });
            }
        }

        self.texture.kind = kind;
        self.texture.pixel_kind = pixel_kind;

        let target = kind.to_texture_target();

        unsafe {
            self.state.set_texture(0, target, self.texture.texture);

            let (type_, format, internal_format) = match pixel_kind {
                PixelKind::F32 => (glow::FLOAT, glow::RED, glow::R32F),
                PixelKind::F16 => (glow::FLOAT, glow::RED, glow::R16F),
                PixelKind::D32 => (glow::FLOAT, glow::DEPTH_COMPONENT, glow::DEPTH_COMPONENT32),
                PixelKind::D16 => (glow::FLOAT, glow::DEPTH_COMPONENT, glow::DEPTH_COMPONENT16),
                PixelKind::D24S8 => (
                    glow::UNSIGNED_INT_24_8,
                    glow::DEPTH_STENCIL,
                    glow::DEPTH24_STENCIL8,
                ),
                PixelKind::RGBA8 => (glow::UNSIGNED_BYTE, glow::RGBA, glow::RGBA8),
                PixelKind::RGB8 => (glow::UNSIGNED_BYTE, glow::RGB, glow::RGB8),
                PixelKind::RG8 => (glow::UNSIGNED_BYTE, glow::RG, glow::RG8),
                PixelKind::R8 => (glow::UNSIGNED_BYTE, glow::RED, glow::R8),
                PixelKind::BGRA8 => (glow::UNSIGNED_BYTE, glow::BGRA, glow::RGBA8),
                PixelKind::BGR8 => (glow::UNSIGNED_BYTE, glow::BGR, glow::RGB8),
                PixelKind::RG16 => (glow::UNSIGNED_SHORT, glow::RG, glow::RG16),
                PixelKind::R16 => (glow::UNSIGNED_SHORT, glow::RED, glow::R16),
                PixelKind::RGB16 => (glow::UNSIGNED_SHORT, glow::RGB, glow::RGB16),
                PixelKind::RGBA16 => (glow::UNSIGNED_SHORT, glow::RGBA, glow::RGBA16),
                PixelKind::DXT1RGB => (0, 0, GL_COMPRESSED_RGB_S3TC_DXT1_EXT),
                PixelKind::DXT1RGBA => (0, 0, GL_COMPRESSED_RGBA_S3TC_DXT1_EXT),
                PixelKind::DXT3RGBA => (0, 0, GL_COMPRESSED_RGBA_S3TC_DXT3_EXT),
                PixelKind::DXT5RGBA => (0, 0, GL_COMPRESSED_RGBA_S3TC_DXT5_EXT),
                PixelKind::R8RGTC => (0, 0, COMPRESSED_RED_RGTC1),
                PixelKind::RG8RGTC => (0, 0, COMPRESSED_RG_RGTC2),
                PixelKind::RGBA32F => (glow::FLOAT, glow::RGBA, glow::RGBA32F),
            };

            let is_compressed = pixel_kind.is_compressed();

            // Compressed textures does not have such term as "unpack alignment", so we have to check
            // for compressed textures here.
            if !is_compressed {
                self.state
                    .gl
                    .pixel_store_i32(glow::UNPACK_ALIGNMENT, pixel_kind.unpack_alignment());
            }

            let mut mip_byte_offset = 0;
            'mip_loop2: for mip in 0..mip_count {
                match kind {
                    GpuTextureKind::Line { length } => {
                        if let Some(length) = length.checked_shr(mip as u32) {
                            let pixels = data.map(|data| &data[mip_byte_offset..]);
                            let size = image_1d_size_bytes(pixel_kind, length) as i32;

                            if is_compressed {
                                self.state.gl.compressed_tex_image_1d(
                                    glow::TEXTURE_1D,
                                    mip as i32,
                                    internal_format as i32,
                                    length as i32,
                                    0,
                                    size,
                                    pixels.ok_or(RendererError::EmptyTextureData)?,
                                );
                            } else {
                                self.state.gl.tex_image_1d(
                                    glow::TEXTURE_1D,
                                    mip as i32,
                                    internal_format as i32,
                                    length as i32,
                                    0,
                                    format,
                                    type_,
                                    pixels,
                                );
                            }

                            mip_byte_offset += size as usize;
                        } else {
                            // No need to add degenerated mips (0x1, 0x2, 4x0, etc).
                            break 'mip_loop2;
                        }
                    }
                    GpuTextureKind::Rectangle { width, height } => {
                        if let (Some(width), Some(height)) = (
                            width.checked_shr(mip as u32),
                            height.checked_shr(mip as u32),
                        ) {
                            let pixels = data.map(|data| &data[mip_byte_offset..]);
                            let size = image_2d_size_bytes(pixel_kind, width, height) as i32;

                            if is_compressed {
                                self.state.gl.compressed_tex_image_2d(
                                    glow::TEXTURE_2D,
                                    mip as i32,
                                    internal_format as i32,
                                    width as i32,
                                    height as i32,
                                    0,
                                    size,
                                    pixels.ok_or(RendererError::EmptyTextureData)?,
                                );
                            } else {
                                self.state.gl.tex_image_2d(
                                    glow::TEXTURE_2D,
                                    mip as i32,
                                    internal_format as i32,
                                    width as i32,
                                    height as i32,
                                    0,
                                    format,
                                    type_,
                                    pixels,
                                );
                            }

                            mip_byte_offset += size as usize;
                        } else {
                            // No need to add degenerated mips (0x1, 0x2, 4x0, etc).
                            break 'mip_loop2;
                        }
                    }
                    GpuTextureKind::Cube { width, height } => {
                        if let (Some(width), Some(height)) = (
                            width.checked_shr(mip as u32),
                            height.checked_shr(mip as u32),
                        ) {
                            let bytes_per_face = image_2d_size_bytes(pixel_kind, width, height);

                            for face in 0..6 {
                                let begin = mip_byte_offset + face * bytes_per_face;
                                let end = mip_byte_offset + (face + 1) * bytes_per_face;
                                let face_pixels = data.map(|data| &data[begin..end]);

                                if is_compressed {
                                    self.state.gl.compressed_tex_image_2d(
                                        glow::TEXTURE_CUBE_MAP_POSITIVE_X + face as u32,
                                        mip as i32,
                                        internal_format as i32,
                                        width as i32,
                                        height as i32,
                                        0,
                                        bytes_per_face as i32,
                                        face_pixels.ok_or(RendererError::EmptyTextureData)?,
                                    );
                                } else {
                                    self.state.gl.tex_image_2d(
                                        glow::TEXTURE_CUBE_MAP_POSITIVE_X + face as u32,
                                        mip as i32,
                                        internal_format as i32,
                                        width as i32,
                                        height as i32,
                                        0,
                                        format,
                                        type_,
                                        face_pixels,
                                    );
                                }
                            }

                            mip_byte_offset += 6 * bytes_per_face as usize;
                        } else {
                            // No need to add degenerated mips (0x1, 0x2, 4x0, etc).
                            break 'mip_loop2;
                        }
                    }
                    GpuTextureKind::Volume {
                        width,
                        height,
                        depth,
                    } => {
                        if let (Some(width), Some(height), Some(depth)) = (
                            width.checked_shr(mip as u32),
                            height.checked_shr(mip as u32),
                            depth.checked_shr(mip as u32),
                        ) {
                            let pixels = data.map(|data| &data[mip_byte_offset..]);
                            let size = image_3d_size_bytes(pixel_kind, width, height, depth) as i32;

                            if is_compressed {
                                self.state.gl.compressed_tex_image_3d(
                                    glow::TEXTURE_3D,
                                    0,
                                    internal_format as i32,
                                    width as i32,
                                    height as i32,
                                    depth as i32,
                                    0,
                                    size,
                                    pixels.ok_or(RendererError::EmptyTextureData)?,
                                );
                            } else {
                                self.state.gl.tex_image_3d(
                                    glow::TEXTURE_3D,
                                    0,
                                    internal_format as i32,
                                    width as i32,
                                    height as i32,
                                    depth as i32,
                                    0,
                                    format,
                                    type_,
                                    pixels,
                                );
                            }

                            mip_byte_offset += size as usize;
                        } else {
                            // No need to add degenerated mips (0x1, 0x2, 4x0, etc).
                            break 'mip_loop2;
                        }
                    }
                }
            }
        }

        Ok(self)
    }
}

const GL_COMPRESSED_RGB_S3TC_DXT1_EXT: u32 = 0x83F0;
const GL_COMPRESSED_RGBA_S3TC_DXT1_EXT: u32 = 0x83F1;
const GL_COMPRESSED_RGBA_S3TC_DXT3_EXT: u32 = 0x83F2;
const GL_COMPRESSED_RGBA_S3TC_DXT5_EXT: u32 = 0x83F3;

impl GpuTexture {
    /// Creates new GPU texture of specified kind. Mip count must be at least 1, it means
    /// that there is only main level of detail.
    ///
    /// # Data layout
    ///
    /// In case of Cube texture, `bytes` should contain all 6 cube faces ordered like so,
    /// +X, -X, +Y, -Y, +Z, -Z. Cube mips must follow one after another.
    ///
    /// Produced texture can be used as render target for framebuffer, in this case `data`
    /// parameter can be None.
    ///
    /// # Compressed textures
    ///
    /// For compressed textures data must contain all mips, where each mip must be 2 times
    /// smaller than previous.
    pub fn new(
        state: &mut PipelineState,
        kind: GpuTextureKind,
        pixel_kind: PixelKind,
        min_filter: MinificationFilter,
        mag_filter: MagnificationFilter,
        mip_count: usize,
        data: Option<&[u8]>,
    ) -> Result<Self, RendererError> {
        let mip_count = mip_count.max(1);

        let target = kind.to_texture_target();

        unsafe {
            let texture = state.gl.create_texture()?;

            let mut result = Self {
                state,
                texture,
                kind,
                min_filter,
                mag_filter,
                s_wrap_mode: WrapMode::Repeat,
                t_wrap_mode: WrapMode::Repeat,
                r_wrap_mode: WrapMode::Repeat,
                anisotropy: 1.0,
                pixel_kind,
                thread_mark: PhantomData,
            };

            TextureBinding {
                state,
                texture: &mut result,
            }
            .set_data(kind, pixel_kind, mip_count, data)?;

            state.gl.tex_parameter_i32(
                target,
                glow::TEXTURE_MAG_FILTER,
                mag_filter.into_gl_value(),
            );
            state.gl.tex_parameter_i32(
                target,
                glow::TEXTURE_MIN_FILTER,
                min_filter.into_gl_value(),
            );

            if min_filter != MinificationFilter::Linear
                && min_filter != MinificationFilter::Nearest
                && mip_count == 1
            {
                state.gl.generate_mipmap(target);
            }

            state.set_texture(0, target, 0);

            Log::writeln(
                MessageKind::Information,
                format!("GL texture {} was created!", texture),
            );

            Ok(result)
        }
    }

    pub fn bind_mut<'a>(
        &'a mut self,
        state: &'a mut PipelineState,
        sampler_index: u32,
    ) -> TextureBinding<'a> {
        state.set_texture(sampler_index, self.kind.to_texture_target(), self.texture);
        TextureBinding {
            state,
            texture: self,
        }
    }

    pub fn bind(&self, state: &mut PipelineState, sampler_index: u32) {
        state.set_texture(sampler_index, self.kind.to_texture_target(), self.texture);
    }

    pub fn kind(&self) -> GpuTextureKind {
        self.kind
    }

    pub fn id(&self) -> u32 {
        self.texture
    }

    pub fn minification_filter(&self) -> MinificationFilter {
        self.min_filter
    }

    pub fn magnification_filter(&self) -> MagnificationFilter {
        self.mag_filter
    }

    pub fn s_wrap_mode(&self) -> WrapMode {
        self.s_wrap_mode
    }

    pub fn t_wrap_mode(&self) -> WrapMode {
        self.t_wrap_mode
    }

    pub fn anisotropy(&self) -> f32 {
        self.anisotropy
    }
}

impl Drop for GpuTexture {
    fn drop(&mut self) {
        unsafe {
            Log::writeln(
                MessageKind::Information,
                format!("GL texture {} was destroyed!", self.texture),
            );

            (*self.state).gl.delete_texture(self.texture);
        }
    }
}
