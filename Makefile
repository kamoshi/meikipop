.DEFAULT_GOAL := help

NIX := nix develop -c
CARGO := $(NIX) cargo
SWIFT := $(NIX) swift
HOST_OS := $(shell uname -s)

NATIVE_MANIFEST := native/Cargo.toml
SLINT_MANIFEST := apps/gui-slint/Cargo.toml
SWIFT_PACKAGE := apps/gui-swift

ifeq ($(HOST_OS),Darwin)
PLATFORM_BUILD_TARGETS := build-swift
PLATFORM_CHECK_TARGETS := check-swift
else
PLATFORM_BUILD_TARGETS :=
PLATFORM_CHECK_TARGETS :=
endif

.PHONY: help run run-slint run-swift build build-native build-slint build-swift \
	check check-native check-slint check-swift test test-native

help:
	@echo "MeikiPop development commands"
	@echo
	@echo "  make run           Run the Slint frontend"
	@echo "  make run-slint     Run the Slint frontend"
	@echo "  make run-swift     Run the Swift frontend (macOS only)"
	@echo "  make build         Build all frontends supported on this OS"
	@echo "  make check         Check all frontends supported on this OS"
	@echo "  make test          Run the native test suite"
	@echo
	@echo "Component targets: build-{native,slint,swift}, check-{native,slint,swift}"

run: run-slint

run-slint:
	$(CARGO) run --manifest-path $(SLINT_MANIFEST)

ifeq ($(HOST_OS),Darwin)
run-swift:
	$(SWIFT) run --package-path $(SWIFT_PACKAGE)
else
run-swift:
	@echo "The Swift frontend is only supported on macOS."
	@false
endif

build: build-native build-slint $(PLATFORM_BUILD_TARGETS)

build-native:
	$(CARGO) build --manifest-path $(NATIVE_MANIFEST)

build-slint:
	$(CARGO) build --manifest-path $(SLINT_MANIFEST)

ifeq ($(HOST_OS),Darwin)
build-swift:
	$(SWIFT) build --package-path $(SWIFT_PACKAGE)
else
build-swift:
	@echo "The Swift frontend is only supported on macOS."
	@false
endif

check: check-native check-slint $(PLATFORM_CHECK_TARGETS)

check-native:
	$(CARGO) check --manifest-path $(NATIVE_MANIFEST) --all-targets

check-slint:
	$(CARGO) check --manifest-path $(SLINT_MANIFEST) --all-targets

# SwiftPM has no check-only command, so compiling is its equivalent validation.
ifeq ($(HOST_OS),Darwin)
check-swift:
	$(SWIFT) build --package-path $(SWIFT_PACKAGE)
else
check-swift:
	@echo "The Swift frontend is only supported on macOS."
	@false
endif

test: test-native

test-native:
	$(CARGO) test --manifest-path $(NATIVE_MANIFEST) --all-targets
