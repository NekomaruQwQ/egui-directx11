use std::{io::BufReader, ptr};

use egui::*;

use jpeg_decoder::Decoder;
use windows::Win32::{
    Foundation::{HMODULE, HWND},
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::*,
        Dxgi::{Common::*, *},
    },
};

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowAttributes, WindowId},
};

fn main() {
    AppRunner::<DemoApp>::run(
        WindowAttributes::default()
            .with_title("egui-directx11")
            .with_inner_size(PhysicalSize::new(1920, 1080)),
    );
}

struct DemoApp {
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain,
    render_target: Option<ID3D11RenderTargetView>,
    egui_ctx: egui::Context,
    egui_renderer: egui_directx11::Renderer,
    egui_winit: egui_winit::State,
    tex: TextureId,
}

trait App: Sized {
    fn on_event(&mut self, window: &Window, event: &WindowEvent);
    fn new(window: &Window) -> Self;
    fn on_exit(&mut self);
}

impl App for DemoApp {
    fn new(window: &Window) -> Self {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let RawWindowHandle::Win32(window_handle) = window
            .window_handle()
            .expect("Failed to get window handle")
            .as_raw()
        else {
            panic!("Unexpected RawWindowHandle variant");
        };

        let (device, device_context, swap_chain) = {
            let PhysicalSize { width, height } = window.inner_size();
            Self::create_device_and_swap_chain(
                HWND(window_handle.hwnd.get() as _),
                width,
                height,
                DXGI_FORMAT_R8G8B8A8_UNORM,
            )
        }
        .expect("Failed to create device and swap chain");

        let render_target = Some(
            Self::create_render_target_for_swap_chain(&device, &swap_chain)
                .expect("Failed to create render target"),
        );

        let egui_ctx = egui::Context::default();
        let mut egui_renderer = egui_directx11::Renderer::new(&device)
            .expect("Failed to create egui renderer");
        let egui_winit = egui_winit::State::new(
            egui_ctx.clone(),
            egui_ctx.viewport_id(),
            &window,
            None,
            None,
            None,
        );

        // Image source: https://www.publicdomainpictures.net/en/view-image.php?image=308608
        let bytes =
            Decoder::new(BufReader::new(&include_bytes!("./1080p.jpg")[..]))
                .decode()
                .unwrap();
        let bytes = Vec::from_iter(bytes.chunks_exact(3).map(|slice| {
            u32::from_le_bytes([slice[0], slice[1], slice[2], 0])
        }));

        let desc = D3D11_TEXTURE2D_DESC {
            Width: 1920 as _,
            Height: 1080 as _,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as _,
            CPUAccessFlags: 0,
            ..Default::default()
        };

        let subresource_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: bytes.as_ptr() as _,
            SysMemPitch: (1920 * 4) as u32,
            SysMemSlicePitch: 0,
        };

        let mut tex = None;
        unsafe {
            device.CreateTexture2D(
                &desc,
                Some(&subresource_data),
                Some(&mut tex),
            )
        }
        .unwrap();

        let tex = tex.unwrap();
        let mut shader_resource_view = None;
        unsafe { device.CreateShaderResourceView(&tex, None, Some(&raw mut shader_resource_view)) }.unwrap();
        let srv = shader_resource_view.unwrap();
        let id = egui_renderer.register_user_texture(srv);

        Self {
            device,
            device_context,
            swap_chain,
            render_target,
            egui_ctx,
            egui_renderer,
            egui_winit,
            tex: id,
        }
    }

    fn on_event(&mut self, window: &Window, event: &WindowEvent) {
        let egui_response = self.egui_winit.on_window_event(&window, event);
        if !egui_response.consumed {
            match event {
                WindowEvent::Resized(new_size) => self.resize(new_size),
                WindowEvent::RedrawRequested => self.render(window),
                _ => (),
            }
        }
    }

    fn on_exit(&mut self) {
        self.egui_renderer.unregister_user_texture(self.tex);
    }
}

impl DemoApp {
    fn render(&mut self, window: &Window) {
        if let Some(render_target) = &self.render_target {
            let egui_input = self.egui_winit.take_egui_input(window);
            let egui_output = self.egui_ctx.run(egui_input, |ctx| {
                CentralPanel::default().show(ctx, |ui| {
                    let image = Image::from_texture((
                        self.tex,
                        Vec2::new(1920.0, 1080.0),
                    ))
                    .shrink_to_fit();
                    ui.add(image);
                });
            });
            let (renderer_output, platform_output, _) =
                egui_directx11::split_output(egui_output);
            self.egui_winit
                .handle_platform_output(window, platform_output);
            unsafe {
                self.device_context.ClearRenderTargetView(
                    render_target,
                    &[0.0, 0.0, 0.0, 1.0],
                );
            }
            let _ = self.egui_renderer.render(
                &self.device_context,
                render_target,
                &self.egui_ctx,
                renderer_output,
            );
            let _ = unsafe { self.swap_chain.Present(1, DXGI_PRESENT(0)) };
        } else {
            unreachable!()
        }
    }

    fn resize(&mut self, new_size: &PhysicalSize<u32>) {
        if let Err(err) = self.resize_swap_chain_and_render_target(
            new_size.width,
            new_size.height,
            DXGI_FORMAT_R8G8B8A8_UNORM,
        ) {
            panic!("Failed to resize framebuffers: {err:?}");
        }
    }

    fn create_device_and_swap_chain(
        window: HWND,
        frame_width: u32,
        frame_height: u32,
        frame_format: DXGI_FORMAT,
    ) -> windows::core::Result<(
        ID3D11Device,
        ID3D11DeviceContext,
        IDXGISwapChain,
    )> {
        let dxgi_factory: IDXGIFactory = unsafe { CreateDXGIFactory() }?;
        let dxgi_adapter: IDXGIAdapter =
            unsafe { dxgi_factory.EnumAdapters(0) }?;

        let mut device = None;
        let mut device_context = None;
        unsafe {
            D3D11CreateDevice(
                &dxgi_adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE(ptr::null_mut()),
                if cfg!(debug_assertions) {
                    D3D11_CREATE_DEVICE_DEBUG
                } else {
                    D3D11_CREATE_DEVICE_FLAG(0)
                },
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut device_context),
            )
        }?;
        let device = device.unwrap();
        let device_context = device_context.unwrap();

        let swap_chain_desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: frame_width,
                Height: frame_height,
                Format: frame_format,
                ..DXGI_MODE_DESC::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: window,
            Windowed: true.into(),
            SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
            Flags: 0,
        };

        let mut swap_chain = None;
        unsafe {
            dxgi_factory.CreateSwapChain(
                &device,
                &swap_chain_desc,
                &mut swap_chain,
            )
        }
        .ok()?;
        let swap_chain = swap_chain.unwrap();

        unsafe {
            dxgi_factory.MakeWindowAssociation(window, DXGI_MWA_NO_ALT_ENTER)
        }?;
        Ok((device, device_context, swap_chain))
    }

    fn create_render_target_for_swap_chain(
        device: &ID3D11Device,
        swap_chain: &IDXGISwapChain,
    ) -> windows::core::Result<ID3D11RenderTargetView> {
        let swap_chain_texture =
            unsafe { swap_chain.GetBuffer::<ID3D11Texture2D>(0) }?;
        let mut render_target = None;
        unsafe {
            device.CreateRenderTargetView(
                &swap_chain_texture,
                None,
                Some(&mut render_target),
            )
        }?;
        Ok(render_target.unwrap())
    }

    fn resize_swap_chain_and_render_target(
        &mut self,
        new_width: u32,
        new_height: u32,
        new_format: DXGI_FORMAT,
    ) -> windows::core::Result<()> {
        self.render_target.take();
        unsafe {
            self.swap_chain.ResizeBuffers(
                2,
                new_width,
                new_height,
                new_format,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        }?;
        self.render_target
            .replace(Self::create_render_target_for_swap_chain(
                &self.device,
                &self.swap_chain,
            )?);
        Ok(())
    }
}

struct AppRunner<T: App> {
    window_attributes: WindowAttributes,
    window: Option<Window>,
    app: Option<T>,
}

impl<T: App> AppRunner<T> {
    fn run(window_attributes: WindowAttributes) {
        let mut runner = Self {
            window_attributes,
            window: None,
            app: None,
        };

        EventLoop::new().unwrap().run_app(&mut runner).unwrap();
    }
}

impl<T: App> ApplicationHandler for AppRunner<T> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(self.window_attributes.clone())
            .expect("Failed to create window");
        self.app = Some(T::new(&window));
        self.window = Some(window);
    }

    fn suspended(&mut self, _: &ActiveEventLoop) {
        self.app.take();
        self.window.take();
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if let Some(window) = self.window.as_ref() {
            if window_id == window.id() {
                if event == WindowEvent::CloseRequested {
                    event_loop.exit()
                } else if let Some(app) = self.app.as_mut() {
                    app.on_event(window, &event);
                }
            }
        }
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        T::on_exit(self.app.as_mut().unwrap());
    }
}