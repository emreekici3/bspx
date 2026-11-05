//! Moving windows between Spaces and switching Spaces at once, with SIP on.
//! Both lean on what macOS does not document, so each caller checks that it
//! worked and has a slower way that does not depend on it.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use super::ffi::*;
use super::tap;

#[cfg_attr(target_os = "macos", link(name = "CoreGraphics", kind = "framework"))]
unsafe extern "C" {
    fn CGEventSetIntegerValueField(event: CFTypeRef, field: u32, value: i64);
    fn CGEventSetDoubleValueField(event: CFTypeRef, field: u32, value: f64);
    fn CGEventPost(tap: u32, event: CFTypeRef);
    fn CGEventGetTimestamp(event: CFTypeRef) -> u64;
    fn CGEventCreateData(allocator: *const c_void, event: CFTypeRef) -> CFTypeRef;
    fn CGEventCreateFromData(allocator: *const c_void, data: CFTypeRef) -> CFTypeRef;
}

#[cfg_attr(target_os = "macos", link(name = "CoreFoundation", kind = "framework"))]
unsafe extern "C" {
    static kCFPreferencesAnyApplication: CFStringRef;
    fn CFPreferencesAppSynchronize(application: CFStringRef) -> u8;
    fn CFPreferencesGetAppBooleanValue(key: CFStringRef, app: CFStringRef, valid: *mut u8) -> u8;
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> CFTypeRef;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFDataGetLength(data: CFTypeRef) -> isize;
}

unsafe extern "C" {
    fn sysctlbyname(
        name: *const c_char,
        value: *mut c_void,
        size: *mut usize,
        new: *const c_void,
        new_size: usize,
    ) -> i32;
    fn mach_absolute_time() -> u64;
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
    fn _dyld_get_image_header(index: u32) -> *const u8;
    fn _dyld_get_image_vmaddr_slide(index: u32) -> isize;
}

const SKYLIGHT: &CStr = c"/System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/SkyLight";
/// Private to SkyLight, so `dlsym` cannot see it, but in its symbol table.
const PERFORM_OPERATION: &CStr = c"__ZL54SLSPerformAsynchronousBridgedWindowManagementOperationP47SLSAsynchronousBridgedWindowManagementOperation";

const SESSION_EVENT_TAP: u32 = 1;
/// The fields of a Dock swipe, as the Dock reads them.
const FIELD_EVENT_TYPE: u32 = 55;
const FIELD_GESTURE_KIND: u32 = 110;
const FIELD_SWIPE_MOTION: u32 = 123;
const FIELD_SWIPE_PROGRESS: u32 = 124;
const FIELD_SWIPE_POSITION: u32 = 125;
const FIELD_SWIPE_VELOCITY: u32 = 129;
const FIELD_PHASE: u32 = 132;
const FIELD_PHASE_ALIAS: u32 = 134;
const FIELD_ZOOM_DELTA: u32 = 138;
const FIELD_TIME: u32 = 169;
const GESTURE: i64 = 29;
const DOCK_CONTROL: i64 = 30;
const DOCK_SWIPE: i64 = 23;
const HORIZONTAL: i64 = 1;
const PHASE_BEGAN: i64 = 1;
const PHASE_CHANGED: i64 = 2;
const PHASE_ENDED: i64 = 4;
/// So fast the Dock skips the slide.
const VELOCITY: f64 = 9999.0;
/// The smallest progress a swipe can report, which leaves nothing to slide.
const LEAST_PROGRESS: f64 = 0.000_016;
/// What a real swipe says of its fingers.
const SWIPE_POSITION: f64 = 0.1;
const ZOOM_DELTA: f64 = 3.0;

/// From macOS 27 on, the Dock reads swipes from their IOHID events, which
/// travel at the end of the event's serialized form.
const FIRST_MODERN_MACOS: i32 = 27;
const IOHID_DATA_TAG: u16 = 4205;
const IOHID_VELOCITY: u32 = 9;
const IOHID_FLUID_GESTURE: u32 = 23;
const DOCK_FLAVOR: u16 = 3;
const FLUID_SIZE: u32 = 40;
const VELOCITY_SIZE: u32 = 28;
/// The first bytes of the serialized form this is written for.
const SERIALIZED_VERSION: [u8; 4] = [0, 0, 0, 2];

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const HEADER_SIZE: usize = 32;
const NCMDS_AT: usize = 16;
const SEGNAME_AT: usize = 8;
const VMADDR_AT: usize = 24;
const FILEOFF_AT: usize = 40;
const SYMOFF_AT: usize = 8;
const NSYMS_AT: usize = 12;
const STROFF_AT: usize = 16;
const NLIST_SIZE: usize = 16;
const N_VALUE_AT: usize = 8;

unsafe fn read_u32(at: *const u8) -> u32 {
    unsafe { at.cast::<u32>().read_unaligned() }
}

unsafe fn read_u64(at: *const u8) -> u64 {
    unsafe { at.cast::<u64>().read_unaligned() }
}

/// The address of `symbol` in the loaded image at `header`, from its symbol
/// table.
///
/// # Safety
/// `header` must point to a whole 64-bit Mach-O image, loaded with `slide`.
unsafe fn find_in_image(header: *const u8, slide: isize, symbol: &CStr) -> Option<usize> {
    unsafe {
        let mut linkedit = None;
        let mut symtab = None;
        let mut command = header.add(HEADER_SIZE);
        for _ in 0..read_u32(header.add(NCMDS_AT)) {
            let kind = read_u32(command);
            let name = CStr::from_ptr(command.add(SEGNAME_AT).cast());
            if kind == LC_SEGMENT_64 && name == c"__LINKEDIT" {
                linkedit = Some(command);
            } else if kind == LC_SYMTAB {
                symtab = Some(command);
            }
            command = command.add(read_u32(command.add(4)) as usize);
        }
        let (linkedit, symtab) = (linkedit?, symtab?);
        let base = read_u64(linkedit.add(VMADDR_AT))
            .wrapping_sub(read_u64(linkedit.add(FILEOFF_AT)))
            .wrapping_add_signed(slide as i64) as usize;
        let table = (base + read_u32(symtab.add(SYMOFF_AT)) as usize) as *const u8;
        let strings = (base + read_u32(symtab.add(STROFF_AT)) as usize) as *const u8;
        (0..read_u32(symtab.add(NSYMS_AT)) as usize).find_map(|i| {
            let entry = table.add(i * NLIST_SIZE);
            let name = CStr::from_ptr(strings.add(read_u32(entry) as usize).cast());
            let value = read_u64(entry.add(N_VALUE_AT));
            (name == symbol && value != 0).then(|| value.wrapping_add_signed(slide as i64) as usize)
        })
    }
}

/// The address of `symbol` in the loaded framework `image`.
fn find_symbol(image: &CStr, symbol: &CStr) -> Option<usize> {
    unsafe {
        (0.._dyld_image_count()).find_map(|i| {
            let name = _dyld_get_image_name(i);
            if name.is_null() || CStr::from_ptr(name) != image {
                return None;
            }
            find_in_image(
                _dyld_get_image_header(i),
                _dyld_get_image_vmaddr_slide(i),
                symbol,
            )
        })
    }
}

/// Asks the window server to move the windows of the CFArray `windows` to
/// `space`, which it does a moment later. Returns whether it could ask.
pub fn move_windows(windows: CFTypeRef, space: u64) -> bool {
    type Perform = unsafe extern "C" fn(Id) -> i64;
    static PERFORM: OnceLock<Option<usize>> = OnceLock::new();
    let perform = PERFORM.get_or_init(|| find_symbol(SKYLIGHT, PERFORM_OPERATION));
    let Some(perform) = *perform else {
        return false;
    };
    unsafe {
        let class = objc_getClass(c"SLSBridgedMoveWindowsToManagedSpaceOperation".as_ptr());
        if class.is_null() {
            return false;
        }
        let send = objc_msgSend as unsafe extern "C" fn();
        let plain: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(send);
        let init: unsafe extern "C" fn(Id, Sel, CFTypeRef, u64) -> Id = std::mem::transmute(send);
        let new = plain(class, sel_registerName(c"alloc".as_ptr()));
        let name = sel_registerName(c"initWithWindows:spaceID:".as_ptr());
        let operation = init(new, name, windows, space);
        if operation.is_null() {
            return false;
        }
        let perform: Perform = std::mem::transmute(perform as *const c_void);
        perform(operation);
        plain(operation, sel_registerName(c"release".as_ptr()));
    }
    true
}

/// 16.16 fixed point. What is not zero stays so.
fn fixed(value: f64) -> i32 {
    let fixed = (value * 65536.0) as i32;
    if fixed == 0 && value != 0.0 {
        if value > 0.0 { 1 } else { -1 }
    } else {
        fixed
    }
}

/// The IOHID events of one phase of a swipe: its gesture, and its velocity
/// if it has one.
fn iohid_events(phase: i64, progress: f64, velocity: f64, timestamp: u64) -> Vec<u8> {
    let with_velocity = velocity != 0.0 || phase == PHASE_ENDED;
    let mut out = Vec::new();
    out.extend(timestamp.to_le_bytes());
    out.extend(0u64.to_le_bytes()); // sender
    out.extend(0u32.to_le_bytes()); // options
    out.extend(0u32.to_le_bytes()); // attributes
    out.extend(if with_velocity { 2u32 } else { 1u32 }.to_le_bytes());

    out.extend(FLUID_SIZE.to_le_bytes());
    out.extend(IOHID_FLUID_GESTURE.to_le_bytes());
    out.extend((((phase & 0xFF) as u32) << 24).to_le_bytes());
    out.extend([0u8; 4]); // depth, reserved
    out.extend(fixed(SWIPE_POSITION).to_le_bytes());
    out.extend([0u8; 8]); // position y, z
    out.extend(0u32.to_le_bytes()); // swipe mask
    out.extend((HORIZONTAL as u16).to_le_bytes());
    out.extend(DOCK_FLAVOR.to_le_bytes());
    out.extend(fixed(progress).to_le_bytes());

    if with_velocity {
        out.extend(VELOCITY_SIZE.to_le_bytes());
        out.extend(IOHID_VELOCITY.to_le_bytes());
        out.extend(0u32.to_le_bytes()); // options
        out.extend([1u8, 0, 0, 0]); // depth, reserved
        out.extend(fixed(velocity).to_le_bytes());
        out.extend([0u8; 8]); // velocity y, z
    }
    out
}

/// A serialized event with IOHID events added to it, or `None` if it is not
/// in the form this knows.
fn with_iohid_events(event: &[u8], iohid: &[u8]) -> Option<Vec<u8>> {
    if !event.starts_with(&SERIALIZED_VERSION) {
        return None;
    }
    let length = u16::try_from(iohid.len()).ok()?;
    let mut out = event.to_vec();
    out.extend(length.to_be_bytes());
    out.extend(IOHID_DATA_TAG.to_be_bytes());
    out.extend(iohid);
    Some(out)
}

/// The major version of macOS.
fn macos_major() -> i32 {
    let mut version = [0u8; 32];
    let mut size = version.len();
    let read = unsafe {
        sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            version.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null(),
            0,
        )
    };
    if read != 0 {
        return 0;
    }
    let text = String::from_utf8_lossy(&version);
    let major = text.split(['.', '\0']).next().unwrap_or("");
    major.parse().unwrap_or(0)
}

fn modern_macos() -> bool {
    static MAJOR: OnceLock<i32> = OnceLock::new();
    *MAJOR.get_or_init(macos_major) >= FIRST_MODERN_MACOS
}

/// The sign that goes right. On macOS 27 that is negative, read through the
/// natural scrolling setting.
fn right_sign() -> f64 {
    if !modern_macos() {
        return 1.0;
    }
    let key = super::cfstring("com.apple.swipescrolldirection");
    let mut valid = 0;
    let natural = unsafe {
        CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
        CFPreferencesGetAppBooleanValue(key.ptr(), kCFPreferencesAnyApplication, &raw mut valid)
    };
    if valid == 0 || natural != 0 {
        -1.0
    } else {
        1.0
    }
}

/// A Dock swipe, with what every kind of macOS needs.
unsafe fn dock_swipe(phase: i64) -> Option<super::Cf> {
    unsafe {
        let event = super::Cf::owned(CGEventCreate(std::ptr::null()))?;
        let e = event.ptr();
        CGEventSetIntegerValueField(e, FIELD_EVENT_TYPE, DOCK_CONTROL);
        CGEventSetIntegerValueField(e, FIELD_GESTURE_KIND, DOCK_SWIPE);
        CGEventSetIntegerValueField(e, FIELD_SWIPE_MOTION, HORIZONTAL);
        CGEventSetIntegerValueField(e, FIELD_PHASE, phase);
        CGEventSetIntegerValueField(e, tap::SOURCE_DATA_FIELD, tap::OWN_SWIPE);
        Some(event)
    }
}

/// One phase of a swipe as macOS 27 wants it. The Dock ignores it without a
/// gesture event following.
unsafe fn post_modern(phase: i64, sign: f64) -> Option<()> {
    unsafe {
        let event = dock_swipe(phase)?;
        let e = event.ptr();
        let progress = sign * LEAST_PROGRESS;
        let velocity = if phase == PHASE_ENDED {
            sign * VELOCITY
        } else {
            0.0
        };
        CGEventSetDoubleValueField(e, FIELD_SWIPE_PROGRESS, progress);
        CGEventSetIntegerValueField(e, FIELD_PHASE_ALIAS, phase);
        CGEventSetDoubleValueField(e, FIELD_ZOOM_DELTA, ZOOM_DELTA);
        CGEventSetDoubleValueField(e, FIELD_TIME, mach_absolute_time() as f64);
        CGEventSetDoubleValueField(e, FIELD_SWIPE_POSITION, SWIPE_POSITION);
        CGEventSetDoubleValueField(e, FIELD_SWIPE_VELOCITY, velocity);

        let data = super::Cf::owned(CGEventCreateData(std::ptr::null(), e))?;
        let bytes = CFDataGetBytePtr(data.ptr());
        let length = usize::try_from(CFDataGetLength(data.ptr())).ok()?;
        let serialized = std::slice::from_raw_parts(bytes, length);
        let mut time = CGEventGetTimestamp(e);
        if time == 0 {
            time = mach_absolute_time();
        }
        let iohid = iohid_events(phase, progress, velocity, time);
        let whole = with_iohid_events(serialized, &iohid)?;
        let size = isize::try_from(whole.len()).ok()?;
        let data = super::Cf::owned(CFDataCreate(std::ptr::null(), whole.as_ptr(), size))?;
        let swipe = super::Cf::owned(CGEventCreateFromData(std::ptr::null(), data.ptr()))?;

        let gesture = super::Cf::owned(CGEventCreate(std::ptr::null()))?;
        CGEventSetIntegerValueField(gesture.ptr(), FIELD_EVENT_TYPE, GESTURE);
        CGEventPost(SESSION_EVENT_TAP, swipe.ptr());
        CGEventPost(SESSION_EVENT_TAP, gesture.ptr());
        Some(())
    }
}

/// One phase of a swipe as macOS before 27 wants it.
unsafe fn post_plain(phase: i64, sign: f64) -> Option<()> {
    unsafe {
        let event = dock_swipe(phase)?;
        let e = event.ptr();
        CGEventSetDoubleValueField(e, FIELD_SWIPE_PROGRESS, sign);
        CGEventSetDoubleValueField(e, FIELD_SWIPE_VELOCITY, sign * VELOCITY);
        CGEventPost(SESSION_EVENT_TAP, e);
        Some(())
    }
}

/// Goes `steps` Spaces along, right if positive, with Dock swipes too fast to
/// show a slide. Returns whether the swipes went out.
pub fn swipe(steps: i32) -> bool {
    let sign = f64::from(steps.signum()) * right_sign();
    let post = if modern_macos() {
        post_modern
    } else {
        post_plain
    };
    (0..steps.abs()).all(|_| {
        [PHASE_BEGAN, PHASE_CHANGED, PHASE_ENDED]
            .iter()
            .all(|phase| unsafe { post(*phase, sign) }.is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(image: &mut [u8], at: usize, bytes: &[u8]) {
        image[at..at + bytes.len()].copy_from_slice(bytes);
    }

    #[test]
    fn fixed_point_keeps_the_smallest_progress() {
        assert_eq!(fixed(1.0), 65536);
        assert_eq!(fixed(LEAST_PROGRESS), 1);
        assert_eq!(fixed(-LEAST_PROGRESS), -1);
        assert_eq!(fixed(0.0), 0);
        assert_eq!(fixed(VELOCITY), 655_294_464);
    }

    #[test]
    fn iohid_events_have_the_expected_form() {
        let began = iohid_events(PHASE_BEGAN, LEAST_PROGRESS, 0.0, 7);
        assert_eq!(began.len(), 28 + FLUID_SIZE as usize);
        assert_eq!(began[..8], 7u64.to_le_bytes());
        assert_eq!(began[24..28], 1u32.to_le_bytes());
        assert_eq!(began[28..32], FLUID_SIZE.to_le_bytes());
        assert_eq!(began[32..36], IOHID_FLUID_GESTURE.to_le_bytes());
        assert_eq!(began[36..40], (1u32 << 24).to_le_bytes());
        let ended = iohid_events(PHASE_ENDED, -LEAST_PROGRESS, -VELOCITY, 7);
        let fluid_end = 28 + FLUID_SIZE as usize;
        assert_eq!(ended.len(), fluid_end + VELOCITY_SIZE as usize);
        assert_eq!(ended[24..28], 2u32.to_le_bytes());
        assert_eq!(ended[fluid_end - 8..fluid_end - 4], [1, 0, 3, 0]);
        assert_eq!(ended[fluid_end - 4..fluid_end], (-1i32).to_le_bytes());
        assert_eq!(ended[fluid_end..fluid_end + 4], VELOCITY_SIZE.to_le_bytes());
        assert_eq!(ended[fluid_end + 12], 1);
    }

    #[test]
    fn iohid_events_are_tagged_on_a_serialized_event() {
        let added = with_iohid_events(&[0, 0, 0, 2, 9], &[5, 6]).unwrap();
        assert_eq!(added, [0, 0, 0, 2, 9, 0, 2, 0x10, 0x6d, 5, 6]);
        assert_eq!(with_iohid_events(&[0, 0, 0, 3], &[5]), None);
    }

    #[test]
    fn a_symbol_is_found_in_a_mach_o_image() {
        let mut image = vec![0u8; 512];
        put(&mut image, NCMDS_AT, &2u32.to_ne_bytes());
        let segment = HEADER_SIZE;
        put(&mut image, segment, &LC_SEGMENT_64.to_ne_bytes());
        put(&mut image, segment + 4, &72u32.to_ne_bytes());
        put(&mut image, segment + SEGNAME_AT, b"__LINKEDIT\0");
        put(&mut image, segment + VMADDR_AT, &0x1000u64.to_ne_bytes());
        put(&mut image, segment + FILEOFF_AT, &0x800u64.to_ne_bytes());
        let symtab = segment + 72;
        put(&mut image, symtab, &LC_SYMTAB.to_ne_bytes());
        put(&mut image, symtab + 4, &24u32.to_ne_bytes());
        put(&mut image, symtab + SYMOFF_AT, &0x100u32.to_ne_bytes());
        put(&mut image, symtab + NSYMS_AT, &2u32.to_ne_bytes());
        put(&mut image, symtab + STROFF_AT, &0x140u32.to_ne_bytes());
        // vmaddr - fileoff + slide is the start of the buffer.
        let slide = image.as_ptr() as isize - 0x800;
        let (table, strings) = (0x100, 0x140);
        put(&mut image, strings + 1, b"_other\0_wanted\0");
        put(&mut image, table, &1u32.to_ne_bytes());
        put(&mut image, table + N_VALUE_AT, &0x10u64.to_ne_bytes());
        put(&mut image, table + NLIST_SIZE, &8u32.to_ne_bytes());
        put(
            &mut image,
            table + NLIST_SIZE + N_VALUE_AT,
            &0x20u64.to_ne_bytes(),
        );
        let found = unsafe { find_in_image(image.as_ptr(), slide, c"_wanted") };
        assert_eq!(found, Some((slide + 0x20) as usize));
        let missing = unsafe { find_in_image(image.as_ptr(), slide, c"_nothing") };
        assert_eq!(missing, None);
    }
}
