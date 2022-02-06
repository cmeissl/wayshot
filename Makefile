BINARY := wayshot
BUILDFLAGS := --release
TARGET_DIR := /usr/local/bin

all: build

build:
	@cargo build $(BUILDFLAGS)
	@cp ./target/release/$(BINARY) ./bin/$(DAEMON_BINARY)

install:
	@mkdir -p $(TARGET_DIR)
	@mkdir -p /etc/$(BINARY)
	@touch /etc/$(BINARY)/$(DAEMON_BINARY)rc
	@cp ./bin/$(BINARY) $(TARGET_DIR)
	@chmod +x $(TARGET_DIR)/$(BINARY)

uninstall:
	@rm $(TARGET_DIR)/$(BINARY)

check:
	@cargo fmt
	@cargo check

clean:
	@cargo clean

setup:
	@mkdir bin
	@rustup install stable
	@rustup default stable

.PHONY: check clean setup all install build
