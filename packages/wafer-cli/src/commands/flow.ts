import { readFile } from "node:fs/promises";
import { log } from "../util/logger.js";

export async function flowValidateCommand(file: string): Promise<void> {
  let json: string;
  try {
    json = await readFile(file, "utf-8");
  } catch (err: any) {
    log.error(`Failed to read file: ${err.message}`);
    process.exitCode = 1;
    return;
  }

  // Parse the JSON
  let flow: any;
  try {
    flow = JSON.parse(json);
  } catch (err: any) {
    log.error(`Invalid JSON: ${err.message}`);
    process.exitCode = 1;
    return;
  }

  const errors: string[] = [];

  // Required fields
  if (!flow.id || typeof flow.id !== "string") {
    errors.push("missing or invalid 'id' (must be a string)");
  }
  if (!flow.name || typeof flow.name !== "string") {
    errors.push("missing or invalid 'name' (must be a string)");
  }
  if (!flow.version || typeof flow.version !== "string") {
    errors.push("missing or invalid 'version' (must be a string)");
  }
  if (!Array.isArray(flow.steps) || flow.steps.length === 0) {
    errors.push("'steps' must be a non-empty array");
    log.error(`Validation failed: ${errors.join("; ")}`);
    process.exitCode = 1;
    return;
  }

  // Collect step IDs
  const stepIds = new Set<string>();
  for (const step of flow.steps) {
    if (!step.id || typeof step.id !== "string") {
      errors.push("step missing 'id'");
      continue;
    }
    if (!step.block || typeof step.block !== "string") {
      errors.push(`step '${step.id}' missing 'block'`);
    }
    if (stepIds.has(step.id)) {
      errors.push(`duplicate step id: '${step.id}'`);
    }
    stepIds.add(step.id);

    // Validate next entries
    if (step.next) {
      if (!Array.isArray(step.next)) {
        errors.push(`step '${step.id}': 'next' must be an array`);
        continue;
      }
      let hasDefault = false;
      for (const entry of step.next) {
        if (!entry.step && !entry.flow) {
          errors.push(
            `step '${step.id}': next entry must have 'step' or 'flow'`
          );
        }
        if (entry.step && !stepIds.has(entry.step) && !flow.steps.some((s: any) => s.id === entry.step)) {
          errors.push(
            `step '${step.id}': next references unknown step '${entry.step}'`
          );
        }
        if (!entry.when) {
          hasDefault = true;
        }
      }
      if (!hasDefault) {
        errors.push(
          `step '${step.id}': next must have a default entry (without 'when')`
        );
      }
    }

    // Validate expressions in input
    if (step.input && typeof step.input === "object") {
      validateExpressions(step.id, step.input, errors);
    }
  }

  if (errors.length > 0) {
    log.error(`Validation failed (${errors.length} error${errors.length > 1 ? "s" : ""}):`);
    for (const err of errors) {
      log.error(`  - ${err}`);
    }
    process.exitCode = 1;
  } else {
    log.success(
      `${file} is valid (${flow.steps.length} step${flow.steps.length > 1 ? "s" : ""})`
    );
  }
}

function validateExpressions(
  stepId: string,
  value: any,
  errors: string[]
): void {
  if (typeof value === "string" && value.startsWith("$.")) {
    // Basic path validation: must have at least one segment after $.
    const rest = value.slice(2);
    if (!rest || rest.startsWith(".") || rest.endsWith(".") || rest.includes("..")) {
      errors.push(`step '${stepId}': invalid path expression '${value}'`);
    }
  } else if (typeof value === "object" && value !== null) {
    for (const v of Object.values(value)) {
      validateExpressions(stepId, v, errors);
    }
  }
}
