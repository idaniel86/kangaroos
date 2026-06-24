use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn task(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item // placeholder — implement in Phase 8
}
