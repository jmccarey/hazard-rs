use std::cell::RefCell;
use std::sync::OnceLock;

use crate::domain::HazardDomain;
use crate::hazard::{HazardGuard, HazardPointer};
use crate::sync::AtomicPtr;

static GLOBAL_DOMAIN: OnceLock<HazardDomain> = OnceLock::new();

pub fn global_domain() -> &'static HazardDomain {
    GLOBAL_DOMAIN.get_or_init(HazardDomain::new)
}

pub fn init_global_domain(domain: HazardDomain) -> Result<(), HazardDomain> {
    GLOBAL_DOMAIN.set(domain)
}

thread_local! {
    static THREAD_HP: RefCell<Option<HazardPointer<'static>>> = const { RefCell::new(None) };
}

fn with_thread_hp<F, R>(f: F) -> R
where
    F: FnOnce(&HazardPointer<'static>) -> R,
{
    THREAD_HP.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(HazardPointer::new(global_domain()));
        }
        f(borrow.as_ref().unwrap())
    })
}

pub fn protect<T>(src: &AtomicPtr<T>) -> Option<HazardGuard<'static, T>> {
    with_thread_hp(|hp| {
        let hp_ptr = hp as *const HazardPointer<'static>;
        unsafe { (*hp_ptr).protect(src) }
    })
}

pub fn protect_ptr<T>(ptr: *mut T) -> Option<HazardGuard<'static, T>> {
    with_thread_hp(|hp| {
        let hp_ptr = hp as *const HazardPointer<'static>;
        unsafe { (*hp_ptr).protect_ptr(ptr) }
    })
}

pub fn retire<T>(ptr: *mut T) {
    global_domain().retire(ptr);
}

pub fn retire_with_deleter<T>(ptr: *mut T, deleter: fn(*mut u8)) {
    global_domain().retire_with_deleter(ptr, deleter);
}

pub fn reclaim() {
    global_domain().reclaim();
}

