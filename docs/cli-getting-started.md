# Getting started with the command line

# Install

Download the `rmsync` binary from the [release page](https://github.com/fmonniot/rmsync/releases). Please be sure to choose the binary associated with your platform. If we do not publish such version, you'll have to build the program from source.

Building `rmsync` from source require the rust toolchain. Instruction to install it can be found [on the official website](https://www.rust-lang.org/tools/install). Once installed, clone the repository and use cargo to build the binary:

```shell
$ git clone git@github.com:fmonniot/rmsync.git
$ cd rmsync
$ cargo build --bin rmsync --release
```

The binary is now available at `target/release/rmsync`. We recommend moving it to a location under your `$PATH` variable.

# Authorize

!> This part is under development and mostly likely isn't working at the moment.

When the program start, it will try to find authorization credentials. If it cannot find them, you'll be prompted instruction to pair the tool with your remarkable account. Simply follow the instruction given by the program. You'll have to log in to your remarkable account online and type the device code into the command line utility, in a similar fashion as registering the remarkable tablet.

# Synchronize

Congratulations, You are now ready to use the `rmsync` tool!

Below is a quick overview of the different features the cli offer. If you do not wish to open this website in the future, simply use `rmsync --help` to get short descriptions of the different command and an exhaustive documentation of the available options.

## FanFiction.net

!> TODO

```sh
$ rmsync ffn <story_id> [chapter_number]
```
