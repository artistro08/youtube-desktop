// Build a signed release and write the `latest.json` the app updates from.
//
// The updater reads one file at a fixed address — the `latest.json` attached to
// this repository's newest release — and that file has to name the version, the
// installer's URL, and the signature of that exact installer. The signature is
// produced at build time from the private key and cannot be pointed at: its
// text goes inside the JSON. This script does that assembly, because doing it by
// hand is how a release ends up shipping last version's signature.
//
// Usage:
//
//   $env:TAURI_SIGNING_PRIVATE_KEY = "$HOME\.tauri\youtube-desktop.key"
//   node scripts/release.mjs
//
// Then publish the tag with both files:
//
//   gh release create vX.Y.Z <msi> <latest.json> --repo artistro08/youtube-desktop
//
// Depends on Cargo and the Tauri CLI: https://v2.tauri.app/
//
// Prints the paths it produced and nothing else. It does not tag, commit or
// publish — releasing is a decision, not a build step.

import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const REPO = "artistro08/youtube-desktop";
const BUNDLE_DIR = "src-tauri/target/release/bundle/msi";

// The one platform this app is built for. Tauri names the key after the target
// triple's OS and architecture.
const PLATFORM = "windows-x86_64";

function version() {
    const config = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
    return config.version;
}

function build() {
    if (!process.env.TAURI_SIGNING_PRIVATE_KEY) {
        throw new Error(
            "TAURI_SIGNING_PRIVATE_KEY is not set. Without it the build produces " +
                "no signature, and an unsigned update is one the app will refuse to install.",
        );
    }

    const result = spawnSync("npm", ["run", "build"], {
        stdio: "inherit",
        shell: process.platform === "win32",
    });

    if (result.status !== 0) throw new Error("The release build failed.");
}

// The bundler names the installer after the product, version and target.
function artifacts() {
    const name = `YouTube_${version()}_x64_en-US.msi`;
    const msi = join(BUNDLE_DIR, name);
    const signature = `${msi}.sig`;

    for (const path of [msi, signature]) {
        if (!existsSync(path)) {
            throw new Error(`${path} is missing. Was the build signed?`);
        }
    }

    return { name, msi, signature };
}

function writeManifest({ name, signature }) {
    const manifest = {
        version: version(),
        pub_date: new Date().toISOString(),
        platforms: {
            [PLATFORM]: {
                // Points at this release's own asset by tag, so the file keeps
                // working after the next release makes it no longer "latest".
                url: `https://github.com/${REPO}/releases/download/v${version()}/${name}`,
                signature: readFileSync(signature, "utf8").trim(),
            },
        },
    };

    const path = join(BUNDLE_DIR, "latest.json");
    writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
    return path;
}

build();
const built = artifacts();
const manifest = writeManifest(built);

console.log(`\nVersion ${version()} is ready to publish:`);
console.log(`  ${built.msi}`);
console.log(`  ${manifest}`);
console.log(
    `\ngh release create v${version()} "${built.msi}" "${manifest}" --repo ${REPO}`,
);
