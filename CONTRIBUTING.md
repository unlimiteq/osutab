# Contributing

Thanks for wanting to help. The most useful thing right now is testing on hardware I don't have.

## Reports

- **Tried it on another tablet, or with osu!stable?** Open a [compatibility report](https://github.com/unlimiteq/osutab/issues/new?template=compatibility-report.yml), whether it worked or not.
- **Found a bug?** Open a [bug report](https://github.com/unlimiteq/osutab/issues/new?template=bug-report.yml). The latency log from that run helps a lot. The Latency tab's **Open the logs folder** button finds it.
- **Found a security problem?** Don't open an issue. See [SECURITY.md](SECURITY.md).

## Code changes

1. For anything bigger than a small fix, open an issue first so we can agree on the approach.
2. Build with `.\build.ps1` and run `cargo test`. The [guide](docs/GUIDE.md#building-from-source) lists what the build needs.
3. Keep tests off the real mouse and keyboard. They must never move the cursor or press keys.
4. If the change is something a player would notice, add a line to the `[Unreleased]` part of [CHANGELOG.md](CHANGELOG.md).
5. Open a pull request against `main`. CI runs the tests and CodeQL on it.

Write commit messages as a short plain sentence saying what changed, such as "Let the side button press up to three keys together".

## Where things are

- `crates/core`: the wire format shared by the tablet and the PC
- `crates/helper`: the helper that runs on the tablet and reads the S Pen
- `crates/host`: the Windows program
- `android`: the tablet app
- `otd-plugin`: the OpenTabletDriver bridge plugin
- `docs/GUIDE.md`: the user guide

By contributing, you agree your work is released under the [MIT License](LICENSE).
