#![warn(clippy::all)]

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, Fields, LitInt, Meta, Variant, parse_macro_input};

#[proc_macro_derive(ErrorCode, attributes(error_code))]
pub fn error_code_derive(input: TokenStream) -> TokenStream {
	let ast = parse_macro_input!(input as DeriveInput);
	let syn::Data::Enum(data_enum) = ast.data else {
		return TokenStream::from(
			syn::Error::new(ast.ident.span(), "Expected an error enum").to_compile_error(),
		);
	};

	let error_enum_name = ast.ident;
	let leading_digit = ast
		.attrs
		.iter()
		.find_map(|it| {
			let Meta::List(ref list) = it.meta else {
				return None;
			};

			if let Some(ident) = list.path.segments.first()
				&& ident.ident.to_string() == "error_code"
			{
				if let Ok(digit) = syn::parse::<LitInt>(list.tokens.clone().into())
					&& let Ok(num) = digit.base10_parse::<u8>()
					&& num <= 9
				{
					return Some(num);
				}
			}

			None
		})
		.unwrap();

	let (variants, codes): (Vec<_>, Vec<_>) = data_enum
		.variants
		.iter()
		.enumerate()
		.filter_map(|(i, variant)| {
			let Variant { ident, fields, .. } = variant;

			let fields = match fields {
				Fields::Named(..) => quote! { { .. } },
				Fields::Unnamed(..) => quote! { (..) },
				Fields::Unit => quote! {},
			};
      let Ok(code) = format!("{leading_digit}{i:0>3}").parse::<u16>() else { return None };
			Some((quote! { #ident #fields }, code))
		})
		.unzip();

	quote! {
		impl ErrorCode for #error_enum_name {
			fn code(&self) -> u16 {
				match self {
					#( Self::#variants => #codes ),*
				}
			}
		}
	}
	.into()
}
