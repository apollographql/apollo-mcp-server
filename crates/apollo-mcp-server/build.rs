#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

//! Build Script for the Apollo MCP Server
//!
//! This mostly compiles all the available telemetry attributes
use quote::__private::TokenStream;
use quote::quote;
use std::collections::hash_map::Keys;
use std::io::Write;
use std::iter::Map;
use std::{
    collections::{HashMap, VecDeque},
    io::Read as _,
};
use syn::parse2;

fn snake_to_pascal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = true;
    for ch in s.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            up = true;
        } else {
            if up {
                out.extend(ch.to_uppercase());
            } else {
                out.push(ch);
            }
            up = false;
        }
    }
    out
}

type TokenStreamBuilder = fn(&Vec<String>) -> TokenStream;

fn generate_const_values_from_keys(
    keys: Keys<Vec<String>, String>,
) -> Map<Keys<Vec<String>, String>, TokenStreamBuilder> {
    keys.map(|key| {
        let ident = key
            .iter()
            .map(|k| k.to_uppercase())
            .collect::<Vec<_>>()
            .join("_");
        let ident = quote::format_ident!("{}", ident);
        let value = key.join(".");

        quote! {
            pub const #ident: &str = #value;
        }
    })
}

fn main() {
    // Parse the telemetry file
    let telemetry: toml::Table = {
        let mut raw = String::new();
        std::fs::File::open("telemetry.toml")
            .expect("could not open telemetry file")
            .read_to_string(&mut raw)
            .expect("could not read telemetry file");

        toml::from_str(&raw).expect("could not parse telemetry file")
    };

    // Generate the keys
    let mut attribute_keys = HashMap::new();
    let mut attribute_enum_values = HashMap::new();
    let mut metric_keys = HashMap::new();
    let mut to_visit =
        VecDeque::from_iter(telemetry.into_iter().map(|(key, val)| (vec![key], val)));
    while let Some((key, value)) = to_visit.pop_front() {
        match value {
            toml::Value::String(val) => {
                if key.contains(&"attribute".to_string()) {
                    let last_key = key.last().unwrap().clone();
                    attribute_enum_values.insert(snake_to_pascal(last_key.as_str()), last_key);
                    attribute_keys.insert(key, val);
                } else {
                    metric_keys.insert(key, val);
                }
            }
            toml::Value::Table(map) => to_visit.extend(
                map.into_iter()
                    .map(|(nested_key, value)| ([key.clone(), vec![nested_key]].concat(), value)),
            ),

            _ => panic!("telemetry values should be string descriptions"),
        };
    }

    println!(
        "{:?} | {:?} | {:?}",
        metric_keys, attribute_keys, attribute_enum_values
    );

    // Write out the generated keys
    let out_dir = std::env::var_os("OUT_DIR").expect("could not retrieve output directory");
    let dest_path = std::path::Path::new(&out_dir).join("telemetry_attributes.rs");
    let mut generated_file =
        std::fs::File::create(&dest_path).expect("could not create generated code file");
    let attribute_keys_len = attribute_keys.len();

    let attribute_enum_keys = attribute_enum_values
        .iter()
        .map(|(enum_value, enum_alias)| {
            let enum_value_ident = quote::format_ident!("{}", enum_value);
            quote! {
                #[serde(alias = #enum_alias)]
                #enum_value_ident
            }
        });

    let attribute_enum_values = attribute_enum_values
        .keys()
        .map(|k| quote::format_ident!("{}", k));
    let attribute_const_values = generate_const_values_from_keys(attribute_keys.keys());
    let metric_const_values = generate_const_values_from_keys(metric_keys.keys());

    let tokens = quote! {
        use schemars::JsonSchema;
        use serde::Deserialize;

        pub const ALL_ATTRS: &[TelemetryAttribute; #attribute_keys_len] = &[#(TelemetryAttribute::#attribute_enum_values),*];

        #[derive(Debug, Deserialize, JsonSchema, Clone, Eq, PartialEq, Hash, Copy)]
        pub enum TelemetryAttribute {
            #(#attribute_enum_keys),*
        }

        #( #attribute_const_values )*

        #( #metric_const_values )*
    };

    let file = parse2(tokens).expect("Could not parse TokenStream");
    let code = prettyplease::unparse(&file);

    write!(generated_file, "{}", code.to_string()).expect("Failed to write generated code");

    // Inform cargo that we only want this to run when either this file or the telemetry
    // one changes.
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=telemetry.toml");
}
