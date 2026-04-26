# zed-rest-client-lsp

Native LSP server for the [zed-rest-client](https://github.com/mikolajsemeniuk/zed-rest-client) Zed extension.

Sends HTTP requests from `.http` and `.rest` files and displays responses in a new editor tab.

## Overview

This is a standalone Rust binary that implements the Language Server Protocol. It is **not meant to be installed manually** — the [zed-rest-client](https://github.com/mikolajsemeniuk/zed-rest-client) extension downloads the appropriate prebuilt binary from this repo's GitHub Releases on first use.

If you're a user looking to send HTTP requests from Zed, install the [extension](https://github.com/mikolajsemeniuk/zed-rest-client) from official zed extensions repository.
