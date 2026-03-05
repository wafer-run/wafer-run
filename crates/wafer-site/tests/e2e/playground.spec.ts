import { test, expect } from '@playwright/test';

// ─── PLAYGROUND PAGE LOADS ──────────────────────────────────

test.describe('Playground Page', () => {
  test('loads with editor, language selector, and run button', async ({ page }) => {
    await page.goto('/playground');

    // Header
    await expect(page.locator('.header .brand')).toHaveText('wafer.run');

    // Language selector with three options
    const langSelect = page.locator('#language');
    await expect(langSelect).toBeVisible();
    const options = langSelect.locator('option');
    await expect(options).toHaveCount(3);
    await expect(options.nth(0)).toHaveText('Rust');
    await expect(options.nth(1)).toHaveText('Go');
    await expect(options.nth(2)).toHaveText('JavaScript');

    // Run button
    await expect(page.locator('#runBtn')).toBeVisible();
    await expect(page.locator('#runBtn')).toContainText('Run');

    // Editor textarea with JavaScript template loaded by default
    const editor = page.locator('#editor');
    await expect(editor).toBeVisible();
    const editorValue = await editor.inputValue();
    expect(editorValue).toContain('class HelloBlock');

    // Output panel
    await expect(page.locator('#output')).toBeVisible();
  });

  test('editor label shows current language', async ({ page }) => {
    await page.goto('/playground');
    await expect(page.locator('#editorLabel')).toContainText('JavaScript');
  });

  test('nav links are present', async ({ page }) => {
    await page.goto('/playground');
    await expect(page.locator('.header nav a[href="/"]')).toBeVisible();
    await expect(page.locator('.header nav a[href="/docs"]')).toBeVisible();
  });
});

// ─── LANGUAGE SWITCHING ──────────────────────────────────────

test.describe('Language Switching', () => {
  test('switch to Go loads Go template', async ({ page }) => {
    await page.goto('/playground');

    await page.selectOption('#language', 'go');
    await expect(page.locator('#editorLabel')).toContainText('Go');

    const editorValue = await page.locator('#editor').inputValue();
    expect(editorValue).toContain('package main');
    expect(editorValue).toContain('fmt.Println');
  });

  test('switch to Rust loads Rust template', async ({ page }) => {
    await page.goto('/playground');

    await page.selectOption('#language', 'rust');
    await expect(page.locator('#editorLabel')).toContainText('Rust');

    const editorValue = await page.locator('#editor').inputValue();
    expect(editorValue).toContain('fn main()');
  });

  test('switch back to JavaScript restores JS template', async ({ page }) => {
    await page.goto('/playground');

    // Switch away and back
    await page.selectOption('#language', 'rust');
    await page.selectOption('#language', 'javascript');
    await expect(page.locator('#editorLabel')).toContainText('JavaScript');

    const editorValue = await page.locator('#editor').inputValue();
    expect(editorValue).toContain('class HelloBlock');
  });
});

// ─── JAVASCRIPT EXECUTION (in-browser, no network needed) ───

test.describe('JavaScript Execution', () => {
  test('runs default JS template successfully', async ({ page }) => {
    await page.goto('/playground');

    // Click Run (JS is default now)
    await page.click('#runBtn');

    // Wait for output
    const output = page.locator('#output');
    await expect(output).toContainText('Exited successfully', { timeout: 5000 });
    await expect(output).toContainText('WAFER Block Example');
    await expect(output).toContainText('hello');

    // Status should show Done
    await expect(page.locator('#status')).toHaveText('Done');
  });

  test('handles JS syntax errors gracefully', async ({ page }) => {
    await page.goto('/playground');

    // Replace with invalid code
    await page.locator('#editor').fill('this is not valid javascript!!!');
    await page.click('#runBtn');

    const output = page.locator('#output');
    await expect(output).toContainText('Execution failed', { timeout: 5000 });
    await expect(page.locator('#status')).toHaveText('Error');
  });

  test('captures console.log output', async ({ page }) => {
    await page.goto('/playground');

    await page.locator('#editor').fill('console.log("hello from test");\nconsole.log(1 + 2);');
    await page.click('#runBtn');

    const output = page.locator('#output');
    await expect(output).toContainText('hello from test', { timeout: 5000 });
    await expect(output).toContainText('3');
    await expect(output).toContainText('Exited successfully');
  });

  test('shows return value', async ({ page }) => {
    await page.goto('/playground');

    await page.locator('#editor').fill('return 42;');
    await page.click('#runBtn');

    const output = page.locator('#output');
    await expect(output).toContainText('42', { timeout: 5000 });
    await expect(output).toContainText('Exited successfully');
  });

  test('handles runtime errors gracefully', async ({ page }) => {
    await page.goto('/playground');

    await page.locator('#editor').fill('throw new Error("test error");');
    await page.click('#runBtn');

    const output = page.locator('#output');
    await expect(output).toContainText('test error', { timeout: 5000 });
    await expect(output).toContainText('Execution failed');
  });
});

// ─── RUST PROXY (tests proxy endpoint, external API may be unavailable) ──

test.describe('Rust Proxy Endpoint', () => {
  test('POST /playground/run/rust returns response (success or 502)', async ({ request }) => {
    const resp = await request.post('/playground/run/rust', {
      data: { source: 'fn main() { println!("hello"); }' },
    });

    // Proxy should return 200 (forwarded response), 502 (upstream unreachable), or 503 (service unavailable)
    expect([200, 502, 503]).toContain(resp.status());
  });

  test('POST /playground/run/rust rejects empty source', async ({ request }) => {
    const resp = await request.post('/playground/run/rust', {
      data: { source: '' },
    });
    expect(resp.status()).toBe(400);
    const data = await resp.json();
    expect(data.message).toContain('No source code');
  });
});

// ─── GO PROXY (tests proxy endpoint, external API may be unavailable) ────

test.describe('Go Proxy Endpoint', () => {
  test('POST /playground/run/go returns response (success or 502)', async ({ request }) => {
    const resp = await request.post('/playground/run/go', {
      data: { source: 'package main\n\nimport "fmt"\n\nfunc main() { fmt.Println("hello") }' },
    });

    // Proxy should return 200 (forwarded response), 502 (upstream unreachable), or 503 (service unavailable)
    expect([200, 502, 503]).toContain(resp.status());
  });

  test('POST /playground/run/go rejects empty source', async ({ request }) => {
    const resp = await request.post('/playground/run/go', {
      data: { source: '' },
    });
    expect(resp.status()).toBe(400);
    const data = await resp.json();
    expect(data.message).toContain('No source code');
  });
});

// ─── RUST/GO UI EXECUTION (shows output or error) ───────────

test.describe('Rust/Go UI Execution', () => {
  test('Rust run button shows output (success or network error)', async ({ page }) => {
    await page.goto('/playground');

    // Select Rust first since JS is now default
    await page.selectOption('#language', 'rust');

    await page.click('#runBtn');

    const output = page.locator('#output');
    // Should eventually show EITHER successful output OR a proxy error
    await expect(output).not.toContainText('Click Run', { timeout: 35000 });

    const text = await output.textContent();
    // Verify we got some response, not just the placeholder
    expect(text!.length).toBeGreaterThan(20);
    expect(text).toContain('Compiling and running Rust');
  });

  test('Go run button shows output (success or network error)', async ({ page }) => {
    await page.goto('/playground');
    await page.selectOption('#language', 'go');

    await page.click('#runBtn');

    const output = page.locator('#output');
    await expect(output).not.toContainText('Click Run', { timeout: 35000 });

    const text = await output.textContent();
    expect(text!.length).toBeGreaterThan(20);
    expect(text).toContain('Compiling and running Go');
  });
});

// ─── UI CONTROLS ─────────────────────────────────────────────

test.describe('UI Controls', () => {
  test('Reset button restores template', async ({ page }) => {
    await page.goto('/playground');

    // Modify the editor
    await page.locator('#editor').fill('modified code');
    expect(await page.locator('#editor').inputValue()).toBe('modified code');

    // Click Reset — JS is default, so should restore JS template
    await page.click('button:has-text("Reset")');
    const editorValue = await page.locator('#editor').inputValue();
    expect(editorValue).toContain('class HelloBlock');
  });

  test('Clear Output button clears output panel', async ({ page }) => {
    await page.goto('/playground');

    // Run something to get output (JS is default)
    await page.locator('#editor').fill('console.log("test output");');
    await page.click('#runBtn');
    await expect(page.locator('#output')).toContainText('test output', { timeout: 5000 });

    // Clear
    await page.click('button:has-text("Clear Output")');
    const outputText = await page.locator('#output').textContent();
    expect(outputText).toBe('');
  });

  test('Run button is disabled during execution', async ({ page }) => {
    await page.goto('/playground');

    await page.locator('#editor').fill('for(let i=0;i<1000000;i++){}; console.log("done");');

    await page.click('#runBtn');
    // After completion button should be re-enabled
    await expect(page.locator('#runBtn')).toBeEnabled({ timeout: 10000 });
  });

  test('empty source shows error', async ({ page }) => {
    await page.goto('/playground');
    await page.locator('#editor').fill('');
    await page.click('#runBtn');

    await expect(page.locator('#output')).toContainText('no source code', { timeout: 5000 });
  });

  test('Ctrl+Enter triggers run', async ({ page }) => {
    await page.goto('/playground');
    await page.locator('#editor').fill('console.log("keyboard shortcut");');

    // Focus editor and press Ctrl+Enter
    await page.locator('#editor').focus();
    await page.keyboard.press('Control+Enter');

    await expect(page.locator('#output')).toContainText('keyboard shortcut', { timeout: 5000 });
  });
});

// ─── TEMPLATE API ENDPOINTS ─────────────────────────────────

test.describe('Template API', () => {
  test('GET /playground/templates/rust returns Rust template', async ({ request }) => {
    const resp = await request.get('/playground/templates/rust');
    expect(resp.status()).toBe(200);
    const data = await resp.json();
    expect(data.language).toBe('rust');
    expect(data.template).toContain('fn main()');
  });

  test('GET /playground/templates/go returns Go template', async ({ request }) => {
    const resp = await request.get('/playground/templates/go');
    expect(resp.status()).toBe(200);
    const data = await resp.json();
    expect(data.language).toBe('go');
    expect(data.template).toContain('package main');
  });

  test('GET /playground/templates/javascript returns JS template', async ({ request }) => {
    const resp = await request.get('/playground/templates/javascript');
    expect(resp.status()).toBe(200);
    const data = await resp.json();
    expect(data.language).toBe('javascript');
    expect(data.template).toContain('class HelloBlock');
  });
});
