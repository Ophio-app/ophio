// SPDX-License-Identifier: Apache-2.0
//! Reviewed, immutable upstream artifacts. Updating a model requires new pins.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    pub name: &'static str,
    pub size_bytes: u64,
    pub sha256: &'static str,
    pub url: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogEntry {
    pub name: &'static str,
    pub languages: &'static [&'static str],
    pub license: &'static str,
    pub source: &'static str,
    pub files: &'static [ModelFile],
}

impl CatalogEntry {
    pub fn size_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size_bytes).sum()
    }
}

macro_rules! model {
    ($name:literal, $rev:literal, $size:literal, $hash:literal, $config_size:literal, $config_hash:literal) => {
        CatalogEntry {
            name: $name,
            languages: &["en"],
            license: "MIT",
            source: concat!(
                "https://huggingface.co/openai/whisper-",
                $name,
                "/tree/",
                $rev
            ),
            files: &[
                ModelFile {
                    name: "model.safetensors",
                    size_bytes: $size,
                    sha256: $hash,
                    url: concat!(
                        "https://huggingface.co/openai/whisper-",
                        $name,
                        "/resolve/",
                        $rev,
                        "/model.safetensors"
                    ),
                },
                ModelFile {
                    name: "config.json",
                    size_bytes: $config_size,
                    sha256: $config_hash,
                    url: concat!(
                        "https://huggingface.co/openai/whisper-",
                        $name,
                        "/resolve/",
                        $rev,
                        "/config.json"
                    ),
                },
                ModelFile {
                    name: "tokenizer.json",
                    size_bytes: 2_405_679,
                    sha256: "5eb60cec1e77aeeb6869a2bb5a8e01a84c3fe5d072d75369343021fe6f5310d0",
                    url: concat!(
                        "https://huggingface.co/openai/whisper-",
                        $name,
                        "/resolve/",
                        $rev,
                        "/tokenizer.json"
                    ),
                },
            ],
        }
    };
}

pub const DEFAULT_MODEL: &str = "tiny.en";

static MODELS: &[CatalogEntry] = &[
    model!(
        "tiny.en",
        "87c7102498dcde7456f24cfd30239ca606ed9063",
        151_060_136,
        "db59695928ded6043adaef491a53ef4e12da9611184d77c53baa691a60b958ad",
        1937,
        "b5366317df280f79e6cc366f259f379674f81a8721d458acf2e00680ccb10b1d"
    ),
    model!(
        "base.en",
        "911407f4214e0e1d82085af863093ec0b66f9cd6",
        290_401_888,
        "d4dd5542fd6a1d35639e21384238f3bfe6c557c849d392b5905d33ee29e71db5",
        1937,
        "160c1df40a60d4ef5a6014d536fd845c33aabfd1f318458ac7f87cfe91ebbe76"
    ),
    model!(
        "small.en",
        "e8727524f962ee844a7319d92be39ac1bd25655a",
        966_992_008,
        "6014ac49b506df900f66f4aca6b0801eed7245594ace97bcaf73e0ae5b863066",
        1943,
        "3441094addfb2a31ef62ea117e95bd6e437bbad8e93d3697fe420a4a6d3603a5"
    ),
];

pub fn catalog() -> &'static [CatalogEntry] {
    MODELS
}

pub(crate) fn reviewed(entry: &CatalogEntry) -> bool {
    MODELS.contains(entry)
}
