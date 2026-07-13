use std::mem;

use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP, HDC,
};

pub unsafe fn create_argb_bitmap(
    width: u32,
    height: u32,
    p_bits: &mut *mut core::ffi::c_void,
) -> HBITMAP {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    CreateDIBSection(
        core::mem::zeroed::<HDC>(),
        &bmi,
        DIB_RGB_COLORS,
        p_bits,
        core::mem::zeroed::<windows::Win32::Foundation::HANDLE>(),
        0,
    )
}
