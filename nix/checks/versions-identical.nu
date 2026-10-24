#!/usr/bin/env nu
# Single version across Cargo workspace and ui package.json (0023).

let version = (open VERSION | str trim)
let cargo_version = (open Cargo.toml | get workspace.package.version)
let ui_version = (open apps/ui/package.json | get version)
let checks = [
    { name: "cargo", value: $cargo_version }
    { name: "ui", value: $ui_version }
]
let mismatches = ($checks | where value != $version)

if ($mismatches | is-not-empty) {
    for m in $mismatches {
        print -e $"version mismatch: VERSION=($version) but ($m.name) has ($m.value)"
    }
    exit 1
}

print $"versions identical: ($version)"
