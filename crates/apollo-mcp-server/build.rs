#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

//! Build Script for the Apollo MCP Server
//!
//! This mostly compiles all the available telemetry attributes
use quote::__private::TokenStream;
use quote::quote;
use serde::Deserialize;
use std::io::Write;
use std::{collections::VecDeque, io::Read as _};
use syn::{Ident, parse2};

#[derive(Deserialize)]
struct TelemetryTomlData {
    attributes: toml::Table,
    metrics: toml::Table,
}

#[derive(Eq, PartialEq, Debug, Clone)]
struct TelemetryData {
    name: String,
    alias: String,
    value: String,
    description: String,
}

fn flatten(table: toml::Table) -> Vec<TelemetryData> {
    let mut to_visit = VecDeque::from_iter(table.into_iter().map(|(key, val)| (vec![key], val)));
    let mut telemetry_data = Vec::new();

    while let Some((key, value)) = to_visit.pop_front() {
        match value {
            toml::Value::String(val) => {
                let last_key = key.last().unwrap().clone();
                telemetry_data.push(TelemetryData {
                    name: cruet::to_pascal_case(last_key.as_str()),
                    alias: last_key,
                    value: key.join("."),
                    description: val,
                });
            }
            toml::Value::Table(map) => to_visit.extend(
                map.into_iter()
                    .map(|(nested_key, value)| ([key.clone(), vec![nested_key]].concat(), value)),
            ),

            _ => panic!("telemetry values should be string descriptions"),
        };
    }

    telemetry_data
}

fn generate_enum(telemetry_data: &[TelemetryData]) -> Vec<TokenStream> {
    telemetry_data
        .iter()
        .map(|t| {
            let enum_value_ident = quote::format_ident!("{}", &t.name);
            let alias = &t.alias;
            let doc_message = &t.description;
            quote! {
                #[doc = #doc_message]
                #[serde(alias = #alias)]
                #enum_value_ident
            }
        })
        .collect::<Vec<_>>()
}

fn generate_enum_as_str_matches(
    telemetry_data: &[TelemetryData],
    enum_ident: Ident,
) -> Vec<TokenStream> {
    telemetry_data
        .iter()
        .map(|t| {
            let name_ident = quote::format_ident!("{}", &t.name);
            let value = &t.value;
            quote! {
                #enum_ident::#name_ident => #value
            }
        })
        .collect::<Vec<_>>()
}

fn main() {
    // Parse the telemetry file
    let telemetry: TelemetryTomlData = {
        let mut raw = String::new();
        std::fs::File::open("telemetry.toml")
            .expect("could not open telemetry file")
            .read_to_string(&mut raw)
            .expect("could not read telemetry file");

        toml::from_str(&raw).expect("could not parse telemetry file")
    };

    // Generate the keys
    let telemetry_attribute_data = flatten(telemetry.attributes);
    let telemetry_metrics_data = flatten(telemetry.metrics);
    println!(
        "a {:?} | m {:?}",
        telemetry_attribute_data, telemetry_metrics_data
    );

    // Write out the generated keys
    let out_dir = std::env::var_os("OUT_DIR").expect("could not retrieve output directory");
    let dest_path = std::path::Path::new(&out_dir).join("telemetry_attributes.rs");
    let mut generated_file =
        std::fs::File::create(&dest_path).expect("could not create generated code file");

    let attribute_keys_len = telemetry_attribute_data.len();
    let attribute_enum_keys = generate_enum(&telemetry_attribute_data);
    let all_attribute_enum_values = &telemetry_attribute_data
        .iter()
        .map(|t| quote::format_ident!("{}", t.name));
    let all_attribute_enum_values = (*all_attribute_enum_values).clone();
    let attribute_enum_name = quote::format_ident!("{}", "TelemetryAttribute");
    let attribute_enum_as_str_matches =
        generate_enum_as_str_matches(&telemetry_attribute_data, attribute_enum_name.clone());

    let metric_enum_name = quote::format_ident!("{}", "TelemetryMetric");
    let metric_enum_keys = generate_enum(&telemetry_metrics_data);
    let metric_enum_as_str_matches =
        generate_enum_as_str_matches(&telemetry_metrics_data, metric_enum_name.clone());

    let tokens = quote! {
        /// All TelemetryAttribute values
        pub const ALL_ATTRS: &[TelemetryAttribute; #attribute_keys_len] = &[#(TelemetryAttribute::#all_attribute_enum_values),*];

        /// Supported telemetry attribute (tags) values
        #[derive(Debug, ::serde::Deserialize, ::schemars::JsonSchema, Clone, Eq, PartialEq, Hash, Copy)]
        pub enum #attribute_enum_name {
            #(#attribute_enum_keys),*
        }

        impl #attribute_enum_name {
            /// Converts TelemetryAttribute to &str
            pub const fn as_str(&self) -> &'static str {
                match self {
                   #(#attribute_enum_as_str_matches),*
                }
            }
        }

        /// Supported telemetry metrics
        #[derive(Debug, ::serde::Deserialize, ::schemars::JsonSchema, Clone, Eq, PartialEq, Hash, Copy)]
        pub enum #metric_enum_name {
            #(#metric_enum_keys),*
        }

        impl #metric_enum_name {
            /// Converts TelemetryMetric to &str
            pub const fn as_str(&self) -> &'static str {
                match self {
                   #(#metric_enum_as_str_matches),*
                }
            }
        }
    };

    let file = parse2(tokens).expect("Could not parse TokenStream");
    let code = prettyplease::unparse(&file);

    write!(generated_file, "{}", code).expect("Failed to write generated code");

    // Inform cargo that we only want this to run when either this file or the telemetry
    // one changes.
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=telemetry.toml");
}
