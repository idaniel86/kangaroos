use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Expr, FnArg, Ident, ItemFn, LitStr, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

// ---------------------------------------------------------------------------
// Argument parsers
// ---------------------------------------------------------------------------

struct TaskArgs {
    priority: Expr,
    stack_size: Expr,
    time_slice: Expr,
    name: Option<LitStr>,
}

/// Parses: `priority = 0, stack_size = 512, time_slice = 10, name = "foo"`
/// All fields are optional; unset fields use their defaults.
impl Parse for TaskArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut priority: Option<Expr> = None;
        let mut stack_size: Option<Expr> = None;
        let mut time_slice: Option<Expr> = None;
        let mut name: Option<LitStr> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "priority" => priority = Some(input.parse()?),
                "stack_size" => stack_size = Some(input.parse()?),
                "time_slice" => time_slice = Some(input.parse()?),
                "name" => name = Some(input.parse()?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown attribute key `{other}`; expected `priority`, `stack_size`, `time_slice`, or `name`"
                        ),
                    ));
                }
            }
            let _ = input.parse::<Token![,]>();
        }

        Ok(TaskArgs {
            priority: priority.unwrap_or_else(|| syn::parse_str("0").unwrap()),
            stack_size: stack_size.unwrap_or_else(|| syn::parse_str("512usize").unwrap()),
            time_slice: time_slice.unwrap_or_else(|| syn::parse_str("10").unwrap()),
            name,
        })
    }
}

struct MainArgs {
    cpu_hz: Expr,
}

/// Parses: `cpu_hz = 8_000_000, max_tasks = 4`
impl Parse for MainArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut cpu_hz: Option<Expr> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "cpu_hz" => cpu_hz = Some(input.parse()?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown attribute key `{other}`; expected `cpu_hz` or `max_tasks`"
                        ),
                    ));
                }
            }
            let _ = input.parse::<Token![,]>();
        }

        Ok(MainArgs {
            cpu_hz: cpu_hz.ok_or_else(|| {
                syn::Error::new(proc_macro2::Span::call_site(), "`cpu_hz` is required")
            })?,
        })
    }
}

// ---------------------------------------------------------------------------
// #[task(priority = N, stack_size = M, time_slice = K, name = "...")]
// ---------------------------------------------------------------------------

/// Attribute macro that turns a task function into an Embassy-style factory.
///
/// Calling the function with its arguments returns a [`SpawnToken`] that can
/// be passed to [`Spawner::spawn`] inside `#[kangaroos::main]`.
///
/// # Arguments
/// | Key          | Default | Description                            |
/// |--------------|---------|----------------------------------------|
/// | `priority`   | `0`     | Scheduling priority (0 = highest)      |
/// | `stack_size` | `512`   | Stack size **in bytes**                |
/// | `time_slice` | `10`    | Round-robin quantum in SysTick ticks   |
/// | `name`       | fn name | Human-readable name stored in the TCB  |
///
/// # Example
/// ```rust,ignore
/// #[kangaroos::task(priority = 1, stack_size = 2048)]
/// fn blink(pin: u32, period_ms: u64) -> ! {
///     loop { /* use pin and period_ms */ }
/// }
///
/// // In #[main]:
/// spawner.spawn(blink(5, 500));
/// ```
#[proc_macro_attribute]
pub fn task(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TaskArgs);
    let func = parse_macro_input!(item as ItemFn);

    let fn_ident = &func.sig.ident;
    let fn_name_str = fn_ident.to_string();
    let upper = fn_name_str.to_uppercase();

    let storage_ident = Ident::new(&format!("__STORAGE_{upper}"), fn_ident.span());
    let impl_ident = Ident::new(&format!("__impl_{fn_name_str}"), fn_ident.span());
    let name_str = args
        .name
        .unwrap_or_else(|| LitStr::new(&fn_name_str, fn_ident.span()));

    let priority = &args.priority;
    let stack_size = &args.stack_size;
    let time_slice = &args.time_slice;
    let stack_words = quote! { (#stack_size as usize) / 4 };

    // Rename the original function to __impl_*.
    let mut impl_func = func.clone();
    impl_func.sig.ident = impl_ident.clone();

    // Collect typed parameters.
    let params: Vec<(proc_macro2::TokenStream, proc_macro2::TokenStream)> = func
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pt) = arg {
                let (p, t) = (&pt.pat, &pt.ty);
                Some((quote! { #p }, quote! { #t }))
            } else {
                None
            }
        })
        .collect();

    if params.is_empty() {
        // ----------------------------------------------------------------
        // No-parameter path
        // ----------------------------------------------------------------
        quote! {
            #impl_func

            static mut #storage_ident: ::kangaroos::TaskStorage<{#stack_words}> =
                ::kangaroos::TaskStorage::new();

            fn #fn_ident() -> ::kangaroos::SpawnToken {
                let __s = unsafe { &mut *::core::ptr::addr_of_mut!(#storage_ident) };
                let __tcb = __s.tcb_ptr();
                let __stack = __s.stack_slice();
                ::kangaroos::SpawnToken::new(
                    __tcb,
                    __stack.as_mut_ptr(),
                    __stack.len(),
                    #priority as u8,
                    #time_slice as u8,
                    #impl_ident,
                    #name_str,
                )
            }
        }
        .into()
    } else {
        // ----------------------------------------------------------------
        // Parameterised path
        // ----------------------------------------------------------------
        let param_pats: Vec<_> = params.iter().map(|(p, _)| p).collect();
        let param_types: Vec<_> = params.iter().map(|(_, t)| t).collect();
        let param_fields: Vec<_> = params.iter().map(|(p, t)| quote! { #p: #t }).collect();

        let params_ident = Ident::new(&format!("__PARAMS_{upper}"), fn_ident.span());
        let entry_ident = Ident::new(&format!("__entry_{fn_name_str}"), fn_ident.span());

        quote! {
            #impl_func

            static mut #params_ident: ::core::mem::MaybeUninit<(#(#param_types,)*)> =
                ::core::mem::MaybeUninit::uninit();

            fn #entry_ident() -> ! {
                // SAFETY: factory writes params before kernel starts; runs once.
                let (#(#param_pats,)*) = unsafe {
                    ::core::ptr::read(::core::ptr::addr_of!(#params_ident)).assume_init()
                };
                #impl_ident(#(#param_pats,)*)
            }

            static mut #storage_ident: ::kangaroos::TaskStorage<{#stack_words}> =
                ::kangaroos::TaskStorage::new();

            fn #fn_ident(#(#param_fields,)*) -> ::kangaroos::SpawnToken {
                // Write params to static storage before handing the token to spawn.
                unsafe {
                    ::core::ptr::addr_of_mut!(#params_ident)
                        .write(::core::mem::MaybeUninit::new((#(#param_pats,)*)));
                }
                let __s = unsafe { &mut *::core::ptr::addr_of_mut!(#storage_ident) };
                let __tcb = __s.tcb_ptr();
                let __stack = __s.stack_slice();
                ::kangaroos::SpawnToken::new(
                    __tcb,
                    __stack.as_mut_ptr(),
                    __stack.len(),
                    #priority as u8,
                    #time_slice as u8,
                    #entry_ident,
                    #name_str,
                )
            }
        }
        .into()
    }
}

// ---------------------------------------------------------------------------
// #[main(cpu_hz = N, max_tasks = M)]
// ---------------------------------------------------------------------------

/// Attribute macro for the kernel entry point.
///
/// Emits a `static mut KERNEL`, wraps the function with `#[entry]`, injects
/// a [`Spawner`] parameter, and appends `k.start(cpu_hz)`.
///
/// `max_tasks` is the number of **user tasks** (idle task is added automatically).
///
/// # Example
/// ```rust,ignore
/// #[kangaroos::main(cpu_hz = 8_000_000, max_tasks = 2)]
/// fn main(spawner: &mut Spawner) {
///     spawner.spawn(heartbeat());
///     spawner.spawn(blink(5, 500));
///     // optional hardware init before start
/// }
/// ```
#[proc_macro_attribute]
pub fn main(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as MainArgs);
    let func = parse_macro_input!(item as ItemFn);

    let cpu_hz = &args.cpu_hz;

    // Extract the spawner parameter name and type from the function signature.
    // The user writes: fn main(spawner: &mut Spawner) { ... }
    // We re-emit the type in the generated `let` binding so the user's
    // `use kangaroos::Spawner` import is actually referenced and not flagged
    // as unused.
    let (spawner_pat, spawner_ty) = match func.sig.inputs.first() {
        Some(FnArg::Typed(pt)) => {
            let pat = pt.pat.clone();
            // Strip the `&mut` so we can write `let mut name: Spawner = ...`
            let ty = match &*pt.ty {
                syn::Type::Reference(r) => *r.elem.clone(),
                other => other.clone(),
            };
            (pat, ty)
        }
        _ => {
            return syn::Error::new(
                func.sig.ident.span(),
                "#[kangaroos::main] requires exactly one parameter for the spawner, \
             e.g. `fn main(spawner: &mut Spawner)`",
            )
            .to_compile_error()
            .into();
        }
    };

    let user_stmts = &func.block.stmts;

    quote! {
        #[::cortex_m_rt::entry]
        fn main() -> ! {
            let mut #spawner_pat: #spawner_ty = ::kangaroos::Spawner;
            #(#user_stmts)*
            ::kangaroos::start(#cpu_hz)
        }
    }
    .into()
}
