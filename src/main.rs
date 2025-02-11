use std::ffi::c_void;

use wgpu::{Device, Queue, Surface, SurfaceConfiguration, TextureFormat};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{
            Direct2D::*, Direct3D::*, Direct3D11::*, DirectComposition::*, Dxgi::*, Gdi::*,
        },
        System::{Com::*, LibraryLoader::*},
        UI::{HiDpi::*, WindowsAndMessaging::*},
    },
};

fn main() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
    }
    let mut window = Window::new()?;
    window.run()
}

struct Window {
    hwnd: HWND,
    device: Option<ID3D11Device>,
    desktop: Option<IDCompositionDesktopDevice>,
    target: Option<IDCompositionTarget>,
    wgpu_instance: wgpu::Instance,
    wgpu_state: Option<SurfaceState>,
}

impl Window {
    fn new() -> Result<Self> {
        let wgpu = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());

        Ok(Window {
            hwnd: Default::default(),
            device: None,
            desktop: None,
            target: None,
            wgpu_instance: wgpu,
            wgpu_state: None,
        })
    }

    fn create_device_resources(&mut self) -> Result<()> {
        unsafe {
            debug_assert!(self.device.is_none());
            let device_3d = create_device_3d()?;
            let device_2d = create_device_2d(&device_3d)?;
            self.device = Some(device_3d);
            let desktop: IDCompositionDesktopDevice = DCompositionCreateDevice2(&device_2d)?;

            // First release any previous target, otherwise `CreateTargetForHwnd` will find the HWND occupied.
            self.target = None;
            let target = desktop.CreateTargetForHwnd(self.hwnd, true)?;

            let root_visual = desktop.CreateVisual()?;
            target.SetRoot(&root_visual)?;

            self.target = Some(target);

            let wgpu_visual = desktop.CreateVisual()?;
            root_visual.AddVisual(&wgpu_visual, false, None)?;

            let mut rect = RECT::default();
            GetClientRect(self.hwnd, &mut rect)?;

            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;

            let state = pollster::block_on(SurfaceState::new(
                &self.wgpu_instance,
                wgpu_visual.as_raw(),
                width as _,
                height as _,
            ));
            self.wgpu_state.replace(state);

            desktop.Commit()?;

            self.desktop = Some(desktop);
            Ok(())
        }
    }

    fn paint_handler(&mut self) -> Result<()> {
        unsafe {
            if let Some(device) = &self.device {
                if cfg!(debug_assertions) {
                    println!("check device");
                }
                device.GetDeviceRemovedReason()?;
            } else {
                if cfg!(debug_assertions) {
                    println!("build device");
                }
                self.create_device_resources()?;
            }

            self.wgpu_state.as_ref().unwrap().clear();

            ValidateRect(self.hwnd, None).ok()?;
        }

        Ok(())
    }

    fn size_handler(&mut self, lparam: LPARAM) {
        let w = loword(lparam.0 as u32) as u32;
        let h = hiword(lparam.0 as u32) as u32;

        if let Some(state) = &mut self.wgpu_state {
            state.surface_config.width = w;
            state.surface_config.height = h;
            state
                .surface
                .configure(&state.device, &state.surface_config);
        }
    }

    fn message_handler(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match message {
                WM_PAINT => {
                    self.paint_handler().unwrap_or_else(|_| {
                        // Device loss can cause rendering to fail and should not be considered fatal.
                        if cfg!(debug_assertions) {
                            println!("WM_PAINT failed");
                        }
                        self.device = None;
                    });
                }
                WM_SIZE => self.size_handler(lparam),
                WM_DESTROY => PostQuitMessage(0),
                _ => return DefWindowProcA(self.hwnd, message, wparam, lparam),
            }
        }

        LRESULT(0)
    }

    fn run(&mut self) -> Result<()> {
        unsafe {
            let instance = GetModuleHandleA(None)?;
            let window_class = s!("window");

            let wc = WNDCLASSA {
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hInstance: instance.into(),
                lpszClassName: window_class,

                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(Self::wndproc),
                ..Default::default()
            };

            let atom = RegisterClassA(&wc);
            debug_assert!(atom != 0);

            let hwnd = CreateWindowExA(
                WS_EX_NOREDIRECTIONBITMAP,
                window_class,
                s!("Sample Window"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE | WS_SIZEBOX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                None,
                Some(self as *mut _ as _),
            )?;

            debug_assert!(!hwnd.is_invalid());
            debug_assert!(hwnd == self.hwnd);
            let mut message = MSG::default();

            while GetMessageA(&mut message, None, 0, 0).into() {
                DispatchMessageA(&message);
            }

            Ok(())
        }
    }

    extern "system" fn wndproc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe {
            if message == WM_NCCREATE {
                let cs = lparam.0 as *const CREATESTRUCTA;
                let this = (*cs).lpCreateParams as *mut Self;
                (*this).hwnd = window;

                SetWindowLongPtrA(window, GWLP_USERDATA, this as _);
            } else {
                let this = GetWindowLongPtrA(window, GWLP_USERDATA) as *mut Self;

                if !this.is_null() {
                    return (*this).message_handler(message, wparam, lparam);
                }
            }

            DefWindowProcA(window, message, wparam, lparam)
        }
    }
}

struct SurfaceState {
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,
    format: TextureFormat,
}

impl SurfaceState {
    async fn new(
        wgpu_instance: &wgpu::Instance,
        visual: *mut c_void,
        width: u32,
        height: u32,
    ) -> Self {
        let surface = unsafe {
            wgpu_instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CompositionVisual(visual))
                .expect("Failed to create surface!")
        };

        let power_pref = wgpu::PowerPreference::default();
        let adapter = wgpu_instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power_pref,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .expect("Failed to find an appropriate adapter");

        let features = wgpu::Features::empty();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    required_features: features,
                    required_limits: Default::default(),
                    memory_hints: Default::default(),
                },
                None,
            )
            .await
            .expect("Failed to create device");

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        let selected_format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let swapchain_format = swapchain_capabilities
            .formats
            .iter()
            .find(|d| **d == selected_format)
            .expect("failed to select proper surface texture format!");

        dbg!(&swapchain_capabilities.alpha_modes);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: *swapchain_format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 0,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        Self {
            surface,
            queue,
            device,
            surface_config,
            format: selected_format,
        }
    }

    fn clear(&self) {
        let surface_texture = self
            .surface
            .get_current_texture()
            .expect("failed to acquire texture");

        let texture_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.format.add_srgb_suffix()),
                ..Default::default()
            });

        let mut encoder = self.device.create_command_encoder(&Default::default());

        // Create the renderpass which will clear the screen.
        let renderpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.,
                        g: 0.,
                        b: 0.,
                        a: 0.5,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // End the renderpass.
        drop(renderpass);

        // Submit the command in the queue to execute
        self.queue.submit([encoder.finish()]);

        surface_texture.present();
    }
}

fn create_device_3d() -> Result<ID3D11Device> {
    let mut device = None;

    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .map(|()| device.unwrap())
    }
}

fn create_device_2d(device_3d: &ID3D11Device) -> Result<ID2D1Device> {
    let dxgi: IDXGIDevice3 = device_3d.cast()?;
    unsafe { D2D1CreateDevice(&dxgi, None) }
}

#[inline(always)]
pub(crate) const fn loword(x: u32) -> u16 {
    (x & 0xffff) as u16
}

#[inline(always)]
const fn hiword(x: u32) -> u16 {
    ((x >> 16) & 0xffff) as u16
}
