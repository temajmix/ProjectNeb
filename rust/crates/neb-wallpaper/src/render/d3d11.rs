//! The D3D11 half: one device, one flip-model swapchain per window, one draw call per frame.
//!
//! Only DXGI composition is used, never GDI: on Windows 11 24H2 `Progman` has no redirection bitmap,
//! and a GDI child of the desktop wallpaper layer is simply not composited. A DXGI flip-model
//! swapchain is, which is the whole reason this crate exists.

use std::ffi::c_void;
use std::mem::size_of;

use neb_core::logging::LogHandle;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob, ID3DInclude,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_WRITE,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer,
    ID3D11ClassLinkage, ID3D11DepthStencilView, ID3D11Device, ID3D11DeviceContext,
    ID3D11InputLayout, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11Resource, ID3D11Texture2D,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2,
    IDXGIOutput, IDXGISwapChain1,
};
use windows::core::{Interface, PCSTR};

use crate::render::scene::Frame;

/// Buffers in the swapchain: two is the flip model's minimum and costs no latency.
const BUFFER_COUNT: u32 = 2;

/// Why the scene cannot be rendered.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// A D3D11, DXGI or compilation call failed.
    #[error("direct3d: {0}")]
    Win32(#[from] windows::core::Error),

    /// `D3DCompile` refused the shader; the message is the compiler's own.
    #[error("the aurora shader did not compile: {0}")]
    Compile(String),

    /// A swapchain operation found no back buffer to draw into.
    #[error("the swapchain has no back buffer")]
    NoTarget,

    /// `Present` failed in a way that means the device is gone.
    #[error("the scene was lost: {0}")]
    Lost(String),
}

/// One window's scene: the shared device state plus this swapchain's buffers and targets.
#[derive(Debug)]
pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    constants: ID3D11Buffer,
    /// The same buffer as the base resource `Map` takes; kept so a frame performs no `cast`.
    constants_resource: ID3D11Resource,
    target: [Option<ID3D11RenderTargetView>; 1],
    constant_slot: [Option<ID3D11Buffer>; 1],
    viewport: [D3D11_VIEWPORT; 1],
    width: u32,
    height: u32,
}

impl Renderer {
    /// Creates the device, the flip-model swapchain for `window` and both shader stages.
    ///
    /// The window must already be inside the desktop: the swapchain binds to the HWND it is given,
    /// and re-parenting it afterwards is what forces the rebuild this method is called for.
    ///
    /// # Errors
    ///
    /// [`RenderError`] when the machine has no usable D3D11 device, when the shader does not
    /// compile, or when DXGI refuses the swapchain.
    pub fn new(
        window: HWND,
        width: u32,
        height: u32,
        log: Option<&LogHandle>,
    ) -> Result<Self, RenderError> {
        let (width, height) = (width.max(1), height.max(1));
        let device = create_device()?;
        let context = unsafe { device.GetImmediateContext() }?;
        let swapchain = create_swapchain(&device, window, width, height)?;
        let target = render_target(&device, &swapchain)?;

        let vertex_code = compile(
            include_str!("shaders/aurora.hlsl"),
            windows::core::s!("vs_main"),
            windows::core::s!("vs_5_0"),
        )?;
        let pixel_code = compile(
            include_str!("shaders/aurora.hlsl"),
            windows::core::s!("ps_main"),
            windows::core::s!("ps_5_0"),
        )?;

        let mut vertex = None;
        // SAFETY: the bytecode comes from `D3DCompile` on this thread and outlives the call; the
        // class linkage is optional and absent, which is what `None` passes.
        unsafe {
            device.CreateVertexShader(&vertex_code, None::<&ID3D11ClassLinkage>, Some(&mut vertex))
        }?;
        let vertex = vertex.ok_or(RenderError::NoTarget)?;

        let mut pixel = None;
        // SAFETY: as above.
        unsafe {
            device.CreatePixelShader(&pixel_code, None::<&ID3D11ClassLinkage>, Some(&mut pixel))
        }?;
        let pixel = pixel.ok_or(RenderError::NoTarget)?;

        let constants = create_constant_buffer(&device)?;
        let constants_resource: ID3D11Resource = constants.cast()?;

        if let Some(log) = log {
            log.info(
                "wallpaper",
                &format!("scene: d3d11 flip-model swapchain {width}x{height}, 2 buffers"),
            );
        }

        let mut renderer = Self {
            device,
            context,
            swapchain,
            vertex,
            pixel,
            constants,
            constants_resource,
            target: [None],
            constant_slot: [None],
            viewport: [D3D11_VIEWPORT::default()],
            width,
            height,
        };

        renderer.target[0] = Some(target);
        // The constant buffer is bound once and never moves, so a frame binds no new state.
        renderer.constant_slot[0] = Some(renderer.constants.clone());
        renderer.update_viewport();
        Ok(renderer)
    }

    /// The size the back buffers currently have.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Builds the scene into the back buffer and presents it.
    ///
    /// # Errors
    ///
    /// [`RenderError::Lost`] when DXGI reports the surface unusable; the caller drops the renderer
    /// and builds a new one, which is what recovers from a display change or a device reset.
    pub fn render(&mut self, frame: &Frame) -> Result<(), RenderError> {
        self.upload(frame)?;

        // SAFETY: every object passed here is owned by `self` and the slices point at fields that
        // outlive the call. `Draw(3, 0)` reads no vertex buffer: the vertex shader builds the
        // triangle from the vertex id.
        unsafe {
            self.context
                .OMSetRenderTargets(Some(&self.target), None::<&ID3D11DepthStencilView>);
            self.context.RSSetViewports(Some(&self.viewport));
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.IASetInputLayout(None::<&ID3D11InputLayout>);
            self.context.VSSetShader(&self.vertex, None);
            self.context.PSSetShader(&self.pixel, None);
            self.context
                .VSSetConstantBuffers(0, Some(&self.constant_slot));
            self.context
                .PSSetConstantBuffers(0, Some(&self.constant_slot));
            self.context.Draw(3, 0);
        }

        let result = unsafe { self.swapchain.Present(0, DXGI_PRESENT(0)) };
        if result.is_err() {
            return Err(RenderError::Lost(format!("{result:?}")));
        }

        Ok(())
    }

    /// Resizes the back buffers after the monitor under the window changed.
    ///
    /// # Errors
    ///
    /// [`RenderError::Win32`] when DXGI refuses the resize, which a caller treats like a lost scene.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }

        // A bound render target keeps the buffers alive; releasing the view is what lets DXGI
        // reallocate them.
        self.target[0] = None;

        // SAFETY: the swapchain is ours, and the view that referred to the old buffers is gone.
        unsafe {
            self.swapchain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }

        self.target[0] = Some(render_target(&self.device, &self.swapchain)?);
        self.width = width;
        self.height = height;
        self.update_viewport();
        Ok(())
    }

    /// Copies one frame of uniforms into the constant buffer, discarding the previous contents.
    fn upload(&mut self, frame: &Frame) -> Result<(), RenderError> {
        debug_assert_eq!(size_of::<Frame>(), 64, "the shader reads four float4s");

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `WriteDiscard` on a dynamic buffer returns a pointer this call owns until
        // `Unmap`; the copy is exactly `size_of::<Frame>()` bytes, which is the buffer's width.
        unsafe {
            self.context.Map(
                &self.constants_resource,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(
                frame as *const Frame as *const u8,
                mapped.pData as *mut u8,
                size_of::<Frame>(),
            );
            self.context.Unmap(&self.constants, 0);
        }

        Ok(())
    }

    fn update_viewport(&mut self) {
        self.viewport[0] = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: self.width as f32,
            Height: self.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
    }
}

/// Creates a hardware device, falling back to WARP only when no GPU driver answers.
fn create_device() -> Result<ID3D11Device, RenderError> {
    let levels = [
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];

    let mut device = None;
    let mut context = None;
    // SAFETY: the slice outlives the call and the two out parameters are `Option` slots the API
    // fills; the SDK version constant is the documented one.
    let result = unsafe {
        D3D11CreateDevice(
            None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    };

    match result {
        Ok(()) => device.ok_or_else(|| RenderError::Lost("no d3d11 device".to_owned())),
        Err(error) => Err(RenderError::Win32(error)),
    }
}

/// Creates a two-buffer flip-model swapchain bound to `window`.
fn create_swapchain(
    device: &ID3D11Device,
    window: HWND,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1, RenderError> {
    let description = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: BUFFER_COUNT,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
    };

    // SAFETY: the device is live and the description is fully initialised; no restriction to a
    // particular output is wanted, hence the null output parameter.
    unsafe {
        let dxgi_device: IDXGIDevice = device.cast()?;
        let adapter = dxgi_device.GetAdapter()?;
        let factory: IDXGIFactory2 = adapter.GetParent()?;

        Ok(factory.CreateSwapChainForHwnd(
            device,
            window,
            &description,
            None,
            None::<&IDXGIOutput>,
        )?)
    }
}

/// Builds a render target view over the swapchain's first back buffer.
fn render_target(
    device: &ID3D11Device,
    swapchain: &IDXGISwapChain1,
) -> Result<ID3D11RenderTargetView, RenderError> {
    // SAFETY: buffer 0 exists for as long as the swapchain has buffers, which is until it is
    // resized - and this view is only built immediately after creation or a resize.
    unsafe {
        let buffer: ID3D11Texture2D = swapchain.GetBuffer(0)?;
        let resource: ID3D11Resource = buffer.cast()?;
        let mut target = None;
        device.CreateRenderTargetView(&resource, None, Some(&mut target))?;

        target.ok_or(RenderError::NoTarget)
    }
}

/// Creates the 64-byte dynamic constant buffer the shader's `b0` block lives in.
fn create_constant_buffer(device: &ID3D11Device) -> Result<ID3D11Buffer, RenderError> {
    let description = D3D11_BUFFER_DESC {
        ByteWidth: size_of::<Frame>() as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };

    // SAFETY: the description is fully initialised and no initial data is wanted.
    unsafe {
        let mut buffer = None;
        device.CreateBuffer(&description, None, Some(&mut buffer))?;

        buffer.ok_or(RenderError::NoTarget)
    }
}

/// Compiles one entry point of the aurora shader into bytecode.
fn compile(source: &str, entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, RenderError> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;

    // SAFETY: the source slice, entry point and profile are valid for the call; the two blob out
    // parameters are owned `Option`s the API fills.
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast::<c_void>(),
            source.len(),
            PCSTR::null(),
            None,
            None::<&ID3DInclude>,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };

    if let Err(error) = result {
        let message = errors
            .map(|blob| blob_text(&blob))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| error.message());

        return Err(RenderError::Compile(message));
    }

    let code =
        code.ok_or_else(|| RenderError::Compile("the compiler returned no bytecode".to_owned()))?;
    Ok(unsafe { blob_bytes(&code) }.to_vec())
}

/// The text of a compiler diagnostic blob, with the trailing NUL removed.
fn blob_text(blob: &ID3DBlob) -> String {
    // SAFETY: the blob owns a NUL-terminated buffer for as long as it is alive; the slice is built
    // from exactly the reported length, so it cannot read past the blob.
    let bytes = unsafe { blob_bytes(blob) };
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_owned()
}

/// The bytes inside a blob.
///
/// # Safety
///
/// `blob` must be a live blob; the returned slice borrows its buffer and is invalidated by the
/// blob's release.
unsafe fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: the caller guarantees the blob is alive; `GetBufferPointer`/`GetBufferSize` describe
    // one contiguous buffer this project never mutates or frees.
    unsafe {
        let pointer = blob.GetBufferPointer();
        let length = blob.GetBufferSize();

        std::slice::from_raw_parts(pointer as *const u8, length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_source_carries_both_entry_points_and_the_uniform_block() {
        let source = include_str!("shaders/aurora.hlsl");

        assert!(source.contains("vs_main"), "no vertex entry point");
        assert!(source.contains("ps_main"), "no pixel entry point");
        assert!(source.contains("register(b0)"), "the uniform block moved");
        assert_eq!(
            source.matches("float4 g_").count(),
            4,
            "the b0 block must stay four float4s, matching scene::Frame"
        );
    }

    #[test]
    fn a_syntax_error_is_reported_with_the_compilers_own_message() {
        let error = compile(
            "this is not hlsl",
            windows::core::s!("ps_main"),
            windows::core::s!("ps_5_0"),
        )
        .expect_err("invalid source must not compile");

        match error {
            RenderError::Compile(message) => {
                assert!(!message.is_empty(), "the refusal must say something");
            }
            other => panic!("expected a compile error, got {other:?}"),
        }
    }
}
