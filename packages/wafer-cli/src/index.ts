#!/usr/bin/env node
import { Command } from "commander";
import { initCommand } from "./commands/init.js";
import { blockCommand } from "./commands/block.js";
import { buildCommand } from "./commands/build.js";
import { devCommand } from "./commands/dev.js";
import { publishCommand } from "./commands/publish.js";
import { typesAddCommand } from "./commands/types.js";

const program = new Command();

program
  .name("wafer")
  .description("CLI for building, running, and publishing Wafer blocks")
  .version("0.1.0");

program
  .command("init")
  .argument("<name>", "project name")
  .description("Create a new Wafer project")
  .action(initCommand);

program
  .command("block")
  .argument("<name>", "block name")
  .option("--lang <language>", "block language (typescript, go, rust)", "typescript")
  .description("Add a new block to the project")
  .action(blockCommand);

program
  .command("build")
  .description("Build all blocks to WebAssembly")
  .action(buildCommand);

program
  .command("dev")
  .option("--port <port>", "dev server port", "8080")
  .description("Build, serve, and watch for changes")
  .action(devCommand);

program
  .command("publish")
  .option("--bump <type>", "version bump type (patch, minor, major)")
  .description("Build and publish blocks as a GitHub release")
  .action(publishCommand);

const typesCmd = program
  .command("types")
  .description("Manage interface type packages");

typesCmd
  .command("add")
  .argument("<ref>", "interface reference (e.g. github.com/owner/repo@v1.0.0)")
  .description("Fetch an interface package and add it to the project")
  .action(typesAddCommand);

program.parseAsync().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exitCode = 1;
});
