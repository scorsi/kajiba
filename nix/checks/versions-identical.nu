#!/usr/bin/env nu
# Single version across Cargo workspace, Go CLI and ui package.json (0023).

let version = (open VERSION | str trim)
let cargo_version = (open Cargo.toml | get workspace.package.version)
let ui_version = (open apps/ui/package.json | get version)
let go_version = (
    open apps/cli/internal/version/version.go
    | lines
    | where ($it | str contains "const Version")
    | first
    | parse 'const Version = "{v}"'
    | get v.0
)

let checks = [
    { name: "cargo", value: $cargo_version }
    { name: "go", value: $go_version }
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
