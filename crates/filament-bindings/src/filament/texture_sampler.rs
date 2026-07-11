use crate::{
    backend::{
        SamplerCompareFunc, SamplerCompareMode, SamplerMagFilter, SamplerMinFilter, SamplerWrapMode,
    },
    bindgen,
};

pub struct TextureSampler {
    native: bindgen::filament_TextureSampler,
}

impl TextureSampler {
    #[inline]
    pub fn new(
        filter_mag: SamplerMagFilter,
        filter_min: SamplerMinFilter,
        wrap_s: SamplerWrapMode,
        wrap_t: SamplerWrapMode,
        wrap_r: SamplerWrapMode,
        anisotropy_log2: u8,
        compare_mode: SamplerCompareMode,
        padding0: u8,
        compare_func: SamplerCompareFunc,
        padding1: u8,
        padding2: u8,
    ) -> Self {
        let mut params = bindgen::filament_backend_SamplerParams::default();
        params.set_filterMag(filter_mag.into());
        params.set_filterMin(filter_min.into());
        params.set_wrapS(wrap_s.into());
        params.set_wrapT(wrap_t.into());
        params.set_wrapR(wrap_r.into());
        params.set_anisotropyLog2(anisotropy_log2);
        params.set_compareMode(compare_mode.into());
        params.set_padding0(padding0);
        params.set_compareFunc(compare_func.into());
        params.set_padding1(padding1);
        params.set_padding2(padding2);
        Self { native: bindgen::filament_TextureSampler { mSamplerParams: params } }
    }

    #[inline]
    pub fn native(&self) -> &bindgen::filament_TextureSampler {
        &self.native
    }

    #[inline]
    pub fn native_params(&self) -> &bindgen::filament_backend_SamplerParams {
        &self.native.mSamplerParams
    }

    #[inline]
    fn native_params_mut(&mut self) -> &mut bindgen::filament_backend_SamplerParams {
        &mut self.native.mSamplerParams
    }

    #[inline]
    pub fn filter_mag(&self) -> SamplerMagFilter {
        SamplerMagFilter::from(self.native_params().filterMag())
    }

    #[inline]
    pub fn set_filter_mag(&mut self, val: SamplerMagFilter) {
        self.native_params_mut().set_filterMag(val.into())
    }

    #[inline]
    pub fn filter_min(&self) -> SamplerMinFilter {
        SamplerMinFilter::from(self.native_params().filterMin())
    }

    #[inline]
    pub fn set_filter_min(&mut self, val: SamplerMinFilter) {
        self.native_params_mut().set_filterMin(val.into())
    }

    #[inline]
    pub fn wrap_s(&self) -> SamplerWrapMode {
        SamplerWrapMode::from(self.native_params().wrapS())
    }

    #[inline]
    pub fn set_wrap_s(&mut self, val: SamplerWrapMode) {
        self.native_params_mut().set_wrapS(val.into())
    }

    #[inline]
    pub fn wrap_tt(&self) -> SamplerWrapMode {
        SamplerWrapMode::from(self.native_params().wrapT())
    }

    #[inline]
    pub fn set_wrap_t(&mut self, val: SamplerWrapMode) {
        self.native_params_mut().set_wrapT(val.into())
    }

    #[inline]
    pub fn wrap_r(&self) -> SamplerWrapMode {
        SamplerWrapMode::from(self.native_params().wrapR())
    }

    #[inline]
    pub fn set_wrap_r(&mut self, val: SamplerWrapMode) {
        self.native_params_mut().set_wrapR(val.into())
    }

    #[inline]
    pub fn anisotropy_log2(&self) -> u8 {
        self.native_params().anisotropyLog2()
    }

    #[inline]
    pub fn set_anisotropy_log2(&mut self, val: u8) {
        self.native_params_mut().set_anisotropyLog2(val)
    }

    #[inline]
    pub fn compare_mode(&self) -> SamplerCompareMode {
        SamplerCompareMode::from(self.native_params().compareMode())
    }

    #[inline]
    pub fn set_compare_mode(&mut self, val: SamplerCompareMode) {
        self.native_params_mut().set_compareMode(val.into())
    }

    #[inline]
    pub fn padding0(&self) -> u8 {
        self.native_params().padding0()
    }

    #[inline]
    pub fn set_padding0(&mut self, val: u8) {
        self.native_params_mut().set_padding0(val)
    }

    #[inline]
    pub fn compare_func(&self) -> SamplerCompareFunc {
        SamplerCompareFunc::from(self.native_params().compareFunc())
    }

    #[inline]
    pub fn set_compare_func(&mut self, val: SamplerCompareFunc) {
        self.native_params_mut().set_compareFunc(val.into())
    }

    #[inline]
    pub fn padding1(&self) -> u8 {
        self.native_params().padding1()
    }

    #[inline]
    pub fn set_padding1(&mut self, val: u8) {
        self.native_params_mut().set_padding1(val)
    }

    #[inline]
    pub fn padding2(&self) -> u8 {
        self.native_params().padding2()
    }

    #[inline]
    pub fn set_padding2(&mut self, val: u8) {
        self.native_params_mut().set_padding2(val)
    }
}

impl Default for TextureSampler {
    fn default() -> Self {
        Self::new(
            SamplerMagFilter::NEAREST,
            SamplerMinFilter::NEAREST,
            SamplerWrapMode::CLAMP_TO_EDGE,
            SamplerWrapMode::CLAMP_TO_EDGE,
            SamplerWrapMode::CLAMP_TO_EDGE,
            0,
            SamplerCompareMode::NONE,
            0,
            SamplerCompareFunc::LE,
            0,
            0,
        )
    }
}
