export function typeScriptBlockTemplate(name: string): string {
  const className = name.charAt(0).toUpperCase() + name.slice(1) + "Block";
  return `import { defineBlock, type Message, type BlockResult, type LifecycleEvent, continueResult, InstanceMode } from 'wafer-sdk-ts';

export default defineBlock({
  info: () => ({
    name: '${name}',
    version: '1.0.0',
    interface: 'processor@v1',
    summary: '${className} block.',
    instanceMode: InstanceMode.PerNode,
    allowedModes: [InstanceMode.PerNode],
  }),

  handle: (msg: Message): BlockResult => {
    return continueResult(msg);
  },

  lifecycle: (_event: LifecycleEvent): void => {},
});
`;
}

export function goBlockTemplate(name: string): string {
  return `package main

import wafer "github.com/wafer-run/wafer-sdk-go"

type ${name}Block struct{}

func (b *${name}Block) Info() wafer.BlockInfo {
	return wafer.BlockInfo{
		Name:    "${name}",
		Version: "1.0.0",
		Interface: "processor@v1",
		Summary: "${name} block.",
	}
}

func (b *${name}Block) Handle(msg *wafer.Message) *wafer.BlockResult {
	return msg.Continue()
}

func (b *${name}Block) Lifecycle(event wafer.LifecycleEvent) error {
	return nil
}

func main() {
	wafer.Export(&${name}Block{})
}
`;
}

export function rustBlockTemplate(name: string): string {
  return `use wafer_sdk::prelude::*;

struct ${name.charAt(0).toUpperCase() + name.slice(1)}Block;

impl Guest for ${name.charAt(0).toUpperCase() + name.slice(1)}Block {
    fn info() -> BlockInfo {
        BlockInfo {
            name: "${name}".into(),
            version: "1.0.0".into(),
            interface: "processor@v1".into(),
            summary: "${name} block.".into(),
            instance_mode: InstanceMode::PerNode,
            allowed_modes: vec![InstanceMode::PerNode],
        }
    }

    fn handle(msg: Message) -> BlockResult {
        msg.cont()
    }

    fn lifecycle(_event: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}

register_block!(${name.charAt(0).toUpperCase() + name.slice(1)}Block);
`;
}

export function blockTemplate(name: string, lang: "typescript" | "go" | "rust"): string {
  switch (lang) {
    case "typescript":
      return typeScriptBlockTemplate(name);
    case "go":
      return goBlockTemplate(name);
    case "rust":
      return rustBlockTemplate(name);
  }
}

export function extensionForLang(lang: "typescript" | "go" | "rust"): string {
  switch (lang) {
    case "typescript":
      return ".ts";
    case "go":
      return ".go";
    case "rust":
      return ".rs";
  }
}
