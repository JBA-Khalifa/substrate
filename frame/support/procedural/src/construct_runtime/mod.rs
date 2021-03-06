// This file is part of Substrate.

// Copyright (C) 2019-2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod parse;

use frame_support_procedural_tools::syn_ext as ext;
use frame_support_procedural_tools::{generate_crate_access, generate_hidden_includes};
use parse::{PalletDeclaration, RuntimeDefinition, WhereSection, PalletPart};
use proc_macro::TokenStream;
use proc_macro2::{TokenStream as TokenStream2};
use quote::quote;
use syn::{Ident, Result, TypePath};
use std::collections::HashMap;

/// The fixed name of the system pallet.
const SYSTEM_PALLET_NAME: &str = "System";

/// The complete definition of a pallet with the resulting fixed index.
#[derive(Debug, Clone)]
pub struct Pallet {
	pub name: Ident,
	pub index: u8,
	pub pallet: Ident,
	pub instance: Option<Ident>,
	pub pallet_parts: Vec<PalletPart>,
}

impl Pallet {
	/// Get resolved pallet parts
	fn pallet_parts(&self) -> &[PalletPart] {
		&self.pallet_parts
	}

	/// Find matching parts
	fn find_part(&self, name: &str) -> Option<&PalletPart> {
		self.pallet_parts.iter().find(|part| part.name() == name)
	}

	/// Return whether pallet contains part
	fn exists_part(&self, name: &str) -> bool {
		self.find_part(name).is_some()
	}
}

/// Convert from the parsed pallet to their final information.
/// Assign index to each pallet using same rules as rust for fieldless enum.
/// I.e. implicit are assigned number incrementedly from last explicit or 0.
fn complete_pallets(decl: impl Iterator<Item = PalletDeclaration>) -> syn::Result<Vec<Pallet>> {
	let mut indices = HashMap::new();
	let mut last_index: Option<u8> = None;
	let mut names = HashMap::new();

	decl
		.map(|pallet| {
			let final_index = match pallet.index {
				Some(i) => i,
				None => last_index.map_or(Some(0), |i| i.checked_add(1))
					.ok_or_else(|| {
						let msg = "Pallet index doesn't fit into u8, index is 256";
						syn::Error::new(pallet.name.span(), msg)
					})?,
			};

			last_index = Some(final_index);

			if let Some(used_pallet) = indices.insert(final_index, pallet.name.clone()) {
				let msg = format!(
					"Pallet indices are conflicting: Both pallets {} and {} are at index {}",
					used_pallet,
					pallet.name,
					final_index,
				);
				let mut err = syn::Error::new(used_pallet.span(), &msg);
				err.combine(syn::Error::new(pallet.name.span(), msg));
				return Err(err);
			}

			if let Some(used_pallet) = names.insert(pallet.name.clone(), pallet.name.span()) {
				let msg = "Two pallets with the same name!";

				let mut err = syn::Error::new(used_pallet, &msg);
				err.combine(syn::Error::new(pallet.name.span(), &msg));
				return Err(err);
			}

			Ok(Pallet {
				name: pallet.name,
				index: final_index,
				pallet: pallet.pallet,
				instance: pallet.instance,
				pallet_parts: pallet.pallet_parts,
			})
		})
		.collect()
}

pub fn construct_runtime(input: TokenStream) -> TokenStream {
	let definition = syn::parse_macro_input!(input as RuntimeDefinition);
	construct_runtime_parsed(definition)
		.unwrap_or_else(|e| e.to_compile_error())
		.into()
}

fn construct_runtime_parsed(definition: RuntimeDefinition) -> Result<TokenStream2> {
	let RuntimeDefinition {
		name,
		where_section: WhereSection {
			block,
			node_block,
			unchecked_extrinsic,
			..
		},
		pallets:
			ext::Braces {
				content: ext::Punctuated { inner: pallets, .. },
				token: pallets_token,
			},
		..
	} = definition;

	let pallets = complete_pallets(pallets.into_iter())?;

	let system_pallet = pallets.iter()
		.find(|decl| decl.name == SYSTEM_PALLET_NAME)
		.ok_or_else(|| syn::Error::new(
			pallets_token.span,
			"`System` pallet declaration is missing. \
			 Please add this line: `System: frame_system::{Pallet, Call, Storage, Config, Event<T>},`",
		))?;

	let hidden_crate_name = "construct_runtime";
	let scrate = generate_crate_access(&hidden_crate_name, "frame-support");
	let scrate_decl = generate_hidden_includes(&hidden_crate_name, "frame-support");

	let all_but_system_pallets = pallets.iter().filter(|pallet| pallet.name != SYSTEM_PALLET_NAME);

	let outer_event = decl_outer_event(
		&name,
		pallets.iter(),
		&scrate,
	)?;

	let outer_origin = decl_outer_origin(
		&name,
		all_but_system_pallets,
		&system_pallet,
		&scrate,
	)?;
	let all_pallets = decl_all_pallets(&name, pallets.iter());
	let pallet_to_index = decl_pallet_runtime_setup(&pallets, &scrate);

	let dispatch = decl_outer_dispatch(&name, pallets.iter(), &scrate);
	let metadata = decl_runtime_metadata(&name, pallets.iter(), &scrate, &unchecked_extrinsic);
	let outer_config = decl_outer_config(&name, pallets.iter(), &scrate);
	let inherent = decl_outer_inherent(
		&name,
		&block,
		&unchecked_extrinsic,
		pallets.iter(),
		&scrate,
	);
	let validate_unsigned = decl_validate_unsigned(&name, pallets.iter(), &scrate);
	let integrity_test = decl_integrity_test(&scrate);

	let res = quote!(
		#scrate_decl

		// Prevent UncheckedExtrinsic to print unused warning.
		const _: () = {
			#[allow(unused)]
			type __hidden_use_of_unchecked_extrinsic = #unchecked_extrinsic;
		};

		#[derive(Clone, Copy, PartialEq, Eq, #scrate::sp_runtime::RuntimeDebug)]
		pub struct #name;
		impl #scrate::sp_runtime::traits::GetNodeBlockType for #name {
			type NodeBlock = #node_block;
		}
		impl #scrate::sp_runtime::traits::GetRuntimeBlockType for #name {
			type RuntimeBlock = #block;
		}

		#outer_event

		#outer_origin

		#all_pallets

		#pallet_to_index

		#dispatch

		#metadata

		#outer_config

		#inherent

		#validate_unsigned

		#integrity_test
	);

	Ok(res)
}

fn decl_validate_unsigned<'a>(
	runtime: &'a Ident,
	pallet_declarations: impl Iterator<Item = &'a Pallet>,
	scrate: &'a TokenStream2,
) -> TokenStream2 {
	let pallets_tokens = pallet_declarations
		.filter(|pallet_declaration| pallet_declaration.exists_part("ValidateUnsigned"))
		.map(|pallet_declaration| &pallet_declaration.name);
	quote!(
		#scrate::impl_outer_validate_unsigned!(
			impl ValidateUnsigned for #runtime {
				#( #pallets_tokens )*
			}
		);
	)
}

fn decl_outer_inherent<'a>(
	runtime: &'a Ident,
	block: &'a syn::TypePath,
	unchecked_extrinsic: &'a syn::TypePath,
	pallet_declarations: impl Iterator<Item = &'a Pallet>,
	scrate: &'a TokenStream2,
) -> TokenStream2 {
	let pallets_tokens = pallet_declarations.filter_map(|pallet_declaration| {
		let maybe_config_part = pallet_declaration.find_part("Inherent");
		maybe_config_part.map(|_| {
			let name = &pallet_declaration.name;
			quote!(#name,)
		})
	});
	quote!(
		#scrate::impl_outer_inherent!(
			impl Inherents where
				Block = #block,
				UncheckedExtrinsic = #unchecked_extrinsic,
				Runtime = #runtime,
			{
				#(#pallets_tokens)*
			}
		);
	)
}

fn decl_outer_config<'a>(
	runtime: &'a Ident,
	pallet_declarations: impl Iterator<Item = &'a Pallet>,
	scrate: &'a TokenStream2,
) -> TokenStream2 {
	let pallets_tokens = pallet_declarations
		.filter_map(|pallet_declaration| {
			pallet_declaration.find_part("Config").map(|part| {
				let transformed_generics: Vec<_> = part
					.generics
					.params
					.iter()
					.map(|param| quote!(<#param>))
					.collect();
				(pallet_declaration, transformed_generics)
			})
		})
		.map(|(pallet_declaration, generics)| {
			let pallet = &pallet_declaration.pallet;
			let name = Ident::new(
				&format!("{}Config", pallet_declaration.name),
				pallet_declaration.name.span(),
			);
			let instance = pallet_declaration.instance.as_ref().into_iter();
			quote!(
				#name =>
					#pallet #(#instance)* #(#generics)*,
			)
		});
	quote!(
		#scrate::impl_outer_config! {
			pub struct GenesisConfig for #runtime where AllPalletsWithSystem = AllPalletsWithSystem {
				#(#pallets_tokens)*
			}
		}
	)
}

fn decl_runtime_metadata<'a>(
	runtime: &'a Ident,
	pallet_declarations: impl Iterator<Item = &'a Pallet>,
	scrate: &'a TokenStream2,
	extrinsic: &TypePath,
) -> TokenStream2 {
	let pallets_tokens = pallet_declarations
		.filter_map(|pallet_declaration| {
			pallet_declaration.find_part("Pallet").map(|_| {
				let filtered_names: Vec<_> = pallet_declaration
					.pallet_parts()
					.iter()
					.filter(|part| part.name() != "Pallet")
					.map(|part| part.ident())
					.collect();
				(pallet_declaration, filtered_names)
			})
		})
		.map(|(pallet_declaration, filtered_names)| {
			let pallet = &pallet_declaration.pallet;
			let name = &pallet_declaration.name;
			let instance = pallet_declaration
				.instance
				.as_ref()
				.map(|name| quote!(<#name>))
				.into_iter();

			let index = pallet_declaration.index;

			quote!(
				#pallet::Pallet #(#instance)* as #name { index #index } with #(#filtered_names)*,
			)
		});
	quote!(
		#scrate::impl_runtime_metadata!{
			for #runtime with pallets where Extrinsic = #extrinsic
				#(#pallets_tokens)*
		}
	)
}

fn decl_outer_dispatch<'a>(
	runtime: &'a Ident,
	pallet_declarations: impl Iterator<Item = &'a Pallet>,
	scrate: &'a TokenStream2,
) -> TokenStream2 {
	let pallets_tokens = pallet_declarations
		.filter(|pallet_declaration| pallet_declaration.exists_part("Call"))
		.map(|pallet_declaration| {
			let pallet = &pallet_declaration.pallet;
			let name = &pallet_declaration.name;
			let index = pallet_declaration.index;
			quote!(#[codec(index = #index)] #pallet::#name)
		});

	quote!(
		#scrate::impl_outer_dispatch! {
			pub enum Call for #runtime where origin: Origin {
				#(#pallets_tokens,)*
			}
		}
	)
}

fn decl_outer_origin<'a>(
	runtime_name: &'a Ident,
	pallets_except_system: impl Iterator<Item = &'a Pallet>,
	system_pallet: &'a Pallet,
	scrate: &'a TokenStream2,
) -> syn::Result<TokenStream2> {
	let mut pallets_tokens = TokenStream2::new();
	for pallet_declaration in pallets_except_system {
		if let Some(pallet_entry) = pallet_declaration.find_part("Origin") {
			let pallet = &pallet_declaration.pallet;
			let instance = pallet_declaration.instance.as_ref();
			let generics = &pallet_entry.generics;
			if instance.is_some() && generics.params.is_empty() {
				let msg = format!(
					"Instantiable pallet with no generic `Origin` cannot \
					 be constructed: pallet `{}` must have generic `Origin`",
					pallet_declaration.name
				);
				return Err(syn::Error::new(pallet_declaration.name.span(), msg));
			}
			let index = pallet_declaration.index;
			let tokens = quote!(#[codec(index = #index)] #pallet #instance #generics,);
			pallets_tokens.extend(tokens);
		}
	}

	let system_name = &system_pallet.pallet;
	let system_index = system_pallet.index;

	Ok(quote!(
		#scrate::impl_outer_origin! {
			pub enum Origin for #runtime_name where
				system = #system_name,
				system_index = #system_index
			{
				#pallets_tokens
			}
		}
	))
}

fn decl_outer_event<'a>(
	runtime_name: &'a Ident,
	pallet_declarations: impl Iterator<Item = &'a Pallet>,
	scrate: &'a TokenStream2,
) -> syn::Result<TokenStream2> {
	let mut pallets_tokens = TokenStream2::new();
	for pallet_declaration in pallet_declarations {
		if let Some(pallet_entry) = pallet_declaration.find_part("Event") {
			let pallet = &pallet_declaration.pallet;
			let instance = pallet_declaration.instance.as_ref();
			let generics = &pallet_entry.generics;
			if instance.is_some() && generics.params.is_empty() {
				let msg = format!(
					"Instantiable pallet with no generic `Event` cannot \
					 be constructed: pallet `{}` must have generic `Event`",
					pallet_declaration.name,
				);
				return Err(syn::Error::new(pallet_declaration.name.span(), msg));
			}

			let index = pallet_declaration.index;
			let tokens = quote!(#[codec(index = #index)] #pallet #instance #generics,);
			pallets_tokens.extend(tokens);
		}
	}

	Ok(quote!(
		#scrate::impl_outer_event! {
			pub enum Event for #runtime_name {
				#pallets_tokens
			}
		}
	))
}

fn decl_all_pallets<'a>(
	runtime: &'a Ident,
	pallet_declarations: impl Iterator<Item = &'a Pallet>,
) -> TokenStream2 {
	let mut types = TokenStream2::new();
	let mut names = Vec::new();
	for pallet_declaration in pallet_declarations {
		let type_name = &pallet_declaration.name;
		let pallet = &pallet_declaration.pallet;
		let mut generics = vec![quote!(#runtime)];
		generics.extend(
			pallet_declaration
				.instance
				.iter()
				.map(|name| quote!(#pallet::#name)),
		);
		let type_decl = quote!(
			pub type #type_name = #pallet::Pallet <#(#generics),*>;
		);
		types.extend(type_decl);
		names.push(&pallet_declaration.name);
	}
	// Make nested tuple structure like (((Babe, Consensus), Grandpa), ...)
	// But ignore the system pallet.
	let all_pallets = names.iter()
		.filter(|n| **n != SYSTEM_PALLET_NAME)
		.fold(TokenStream2::default(), |combined, name| quote!((#name, #combined)));

	let all_pallets_with_system = names.iter()
		.fold(TokenStream2::default(), |combined, name| quote!((#name, #combined)));

	quote!(
		#types
		/// All pallets included in the runtime as a nested tuple of types.
		/// Excludes the System pallet.
		pub type AllPallets = ( #all_pallets );
		/// All pallets included in the runtime as a nested tuple of types.
		pub type AllPalletsWithSystem = ( #all_pallets_with_system );

		/// All modules included in the runtime as a nested tuple of types.
		/// Excludes the System pallet.
		#[deprecated(note = "use `AllPallets` instead")]
		#[allow(dead_code)]
		pub type AllModules = ( #all_pallets );
		/// All modules included in the runtime as a nested tuple of types.
		#[deprecated(note = "use `AllPalletsWithSystem` instead")]
		#[allow(dead_code)]
		pub type AllModulesWithSystem = ( #all_pallets_with_system );
	)
}

fn decl_pallet_runtime_setup(
	pallet_declarations: &[Pallet],
	scrate: &TokenStream2,
) -> TokenStream2 {
	let names = pallet_declarations.iter().map(|d| &d.name);
	let names2 = pallet_declarations.iter().map(|d| &d.name);
	let name_strings = pallet_declarations.iter().map(|d| d.name.to_string());
	let indices = pallet_declarations.iter()
		.map(|pallet| pallet.index as usize);

	quote!(
		/// Provides an implementation of `PalletInfo` to provide information
		/// about the pallet setup in the runtime.
		pub struct PalletInfo;

		impl #scrate::traits::PalletInfo for PalletInfo {
			fn index<P: 'static>() -> Option<usize> {
				let type_id = #scrate::sp_std::any::TypeId::of::<P>();
				#(
					if type_id == #scrate::sp_std::any::TypeId::of::<#names>() {
						return Some(#indices)
					}
				)*

				None
			}

			fn name<P: 'static>() -> Option<&'static str> {
				let type_id = #scrate::sp_std::any::TypeId::of::<P>();
				#(
					if type_id == #scrate::sp_std::any::TypeId::of::<#names2>() {
						return Some(#name_strings)
					}
				)*

				None
			}
		}
	)
}

fn decl_integrity_test(scrate: &TokenStream2) -> TokenStream2 {
	quote!(
		#[cfg(test)]
		mod __construct_runtime_integrity_test {
			use super::*;

			#[test]
			pub fn runtime_integrity_tests() {
				<AllPallets as #scrate::traits::IntegrityTest>::integrity_test();
			}
		}
	)
}
