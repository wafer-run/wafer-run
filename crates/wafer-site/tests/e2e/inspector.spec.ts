import { test, expect } from '@playwright/test';

// ─── INSPECTOR PAGE LOADING ────────────────────────────────

test.describe('Inspector Page', () => {
  test('loads and shows Blocks tab by default', async ({ page }) => {
    const resp = await page.goto('/_inspector/ui');
    expect(resp?.status()).toBe(200);

    // Header is present
    await expect(page.locator('.header h1')).toContainText('WAFER');
    await expect(page.locator('.header h1')).toContainText('Inspector');

    // Blocks tab is active
    await expect(page.locator('.tab[data-tab="blocks"]')).toHaveClass(/active/);

    // Blocks view is visible
    await expect(page.locator('#blocks-view')).toBeVisible();

    // Flows view is hidden
    await expect(page.locator('#flows-view')).not.toBeVisible();

    // Intro text is shown
    await expect(page.locator('#blocks-view .intro')).toContainText('Blocks are reusable processing units');

    // Search bar is present
    await expect(page.locator('#block-search')).toBeVisible();
  });

  test('block cards render with name and summary', async ({ page }) => {
    await page.goto('/_inspector/ui');

    // Wait for blocks to load (loading text disappears)
    await page.waitForFunction(() => {
      const el = document.getElementById('block-cards');
      return el && !el.textContent?.includes('Loading');
    }, null, { timeout: 10000 });

    // At least one card should be present (the inspector block itself)
    const cards = page.locator('#block-cards .card');
    await expect(cards.first()).toBeVisible();

    // Each card shows a name (h3) and summary
    const firstCard = cards.first();
    await expect(firstCard.locator('h3')).toBeVisible();
    await expect(firstCard.locator('.summary')).toBeVisible();
  });

  test('block search filters cards', async ({ page }) => {
    await page.goto('/_inspector/ui');

    // Wait for blocks to load
    await page.waitForFunction(() => {
      const el = document.getElementById('block-cards');
      return el && !el.textContent?.includes('Loading');
    }, null, { timeout: 10000 });

    const allCards = await page.locator('#block-cards .card').count();
    expect(allCards).toBeGreaterThan(0);

    // Search for "inspector" — should keep matching cards
    await page.fill('#block-search', 'inspector');
    const filtered = await page.locator('#block-cards .card').count();
    expect(filtered).toBeGreaterThan(0);
    expect(filtered).toBeLessThanOrEqual(allCards);

    // Search for nonsense — should show no cards
    await page.fill('#block-search', 'zzz_nonexistent_block_xyz');
    await expect(page.locator('#block-cards .card')).toHaveCount(0);
    await expect(page.locator('#block-cards')).toContainText('No blocks registered');
  });

  test('expanding a card shows version, interface, and instance details', async ({ page }) => {
    await page.goto('/_inspector/ui');

    // Wait for blocks to load
    await page.waitForFunction(() => {
      const el = document.getElementById('block-cards');
      return el && !el.textContent?.includes('Loading');
    }, null, { timeout: 10000 });

    const firstCard = page.locator('#block-cards .card').first();

    // Details should be hidden initially
    await expect(firstCard.locator('.details')).not.toBeVisible();

    // Click the card header to expand
    await firstCard.locator('[data-card-toggle]').click();

    // Card should now have 'expanded' class
    await expect(firstCard).toHaveClass(/expanded/);

    // Details are now visible with version, interface badge, instance badge
    await expect(firstCard.locator('.details')).toBeVisible();
    await expect(firstCard.locator('.details')).toContainText('Version');
    await expect(firstCard.locator('.badge-interface')).toBeVisible();
    await expect(firstCard.locator('.badge-instance')).toBeVisible();

    // Click again to collapse
    await firstCard.locator('[data-card-toggle]').click();
    await expect(firstCard).not.toHaveClass(/expanded/);
    await expect(firstCard.locator('.details')).not.toBeVisible();
  });
});

// ─── FLOWS TAB ─────────────────────────────────────────────

test.describe('Inspector Flows Tab', () => {
  test('switching to Flows tab shows flow list', async ({ page }) => {
    await page.goto('/_inspector/ui');

    // Click Flows tab
    await page.locator('.tab[data-tab="flows"]').click();

    // Flows tab is active
    await expect(page.locator('.tab[data-tab="flows"]')).toHaveClass(/active/);
    await expect(page.locator('.tab[data-tab="blocks"]')).not.toHaveClass(/active/);

    // Flows view is visible, blocks view is hidden
    await expect(page.locator('#flows-view')).toBeVisible();
    await expect(page.locator('#blocks-view')).not.toBeVisible();

    // Intro text is shown
    await expect(page.locator('#flows-view .intro')).toContainText('Flows wire blocks together');

    // Flow list area is visible
    await expect(page.locator('#flow-list')).toBeVisible();

    // Empty state message shown before selecting a flow
    await expect(page.locator('#flow-empty')).toContainText('Select a flow to visualize');
  });

  test('clicking a flow renders the horizontal flow', async ({ page }) => {
    await page.goto('/_inspector/ui');
    await page.locator('.tab[data-tab="flows"]').click();

    // Wait for flow list to load
    await page.waitForFunction(() => {
      const el = document.getElementById('flow-list');
      return el && !el.textContent?.includes('Loading');
    }, null, { timeout: 10000 });

    const flowItems = page.locator('.flow-item');
    const count = await flowItems.count();

    if (count === 0) {
      // No flows configured — just verify empty state
      test.skip();
      return;
    }

    // Click the first flow
    await flowItems.first().click();
    await expect(flowItems.first()).toHaveClass(/active/);

    // Wait for flow to render
    await page.waitForSelector('.flow-tree', { timeout: 5000 });

    // Flow tree should contain at least one node
    await expect(page.locator('.flow-node').first()).toBeVisible();

    // Empty state should be hidden
    await expect(page.locator('#flow-empty')).not.toBeVisible();
  });

  test('clicking a node in the flow opens the detail panel', async ({ page }) => {
    await page.goto('/_inspector/ui');
    await page.locator('.tab[data-tab="flows"]').click();

    // Wait for flow list to load
    await page.waitForFunction(() => {
      const el = document.getElementById('flow-list');
      return el && !el.textContent?.includes('Loading');
    }, null, { timeout: 10000 });

    const flowItems = page.locator('.flow-item');
    const count = await flowItems.count();
    if (count === 0) { test.skip(); return; }

    // Click the first flow
    await flowItems.first().click();
    await page.waitForSelector('.flow-node', { timeout: 5000 });

    // Detail panel should be closed initially
    await expect(page.locator('#detail-panel')).not.toHaveClass(/open/);

    // Click the first flow node
    await page.locator('.flow-node').first().click();

    // Detail panel should open
    await expect(page.locator('#detail-panel')).toHaveClass(/open/);

    // Panel should show node info
    await expect(page.locator('#panel-content h3')).toBeVisible();
    await expect(page.locator('#panel-content')).toContainText('Node Configuration');

    // Close panel
    await page.locator('#panel-close').click();
    await expect(page.locator('#detail-panel')).not.toHaveClass(/open/);
  });

  test('switching between flows re-renders correctly (no stale state)', async ({ page }) => {
    await page.goto('/_inspector/ui');
    await page.locator('.tab[data-tab="flows"]').click();

    // Wait for flow list to load
    await page.waitForFunction(() => {
      const el = document.getElementById('flow-list');
      return el && !el.textContent?.includes('Loading');
    }, null, { timeout: 10000 });

    const flowItems = page.locator('.flow-item');
    const count = await flowItems.count();
    if (count < 2) { test.skip(); return; }

    // Click first flow
    await flowItems.nth(0).click();
    await page.waitForSelector('.flow-tree', { timeout: 5000 });
    const firstNodes = await page.locator('.flow-node').count();

    // Open detail panel on first node
    await page.locator('.flow-node').first().click();
    await expect(page.locator('#detail-panel')).toHaveClass(/open/);

    // Switch to second flow — panel should close, flow should re-render
    await flowItems.nth(1).click();
    await page.waitForSelector('.flow-tree', { timeout: 5000 });

    // Detail panel should be closed after flow switch
    await expect(page.locator('#detail-panel')).not.toHaveClass(/open/);

    // Flow should have rendered (at least one node)
    await expect(page.locator('.flow-node').first()).toBeVisible();

    // No previously-selected node should remain
    await expect(page.locator('.flow-node.selected')).toHaveCount(0);

    // Switch back to first flow — should render correctly
    await flowItems.nth(0).click();
    await page.waitForSelector('.flow-tree', { timeout: 5000 });
    await expect(page.locator('.flow-node').first()).toBeVisible();
  });
});

// ─── API ENDPOINTS ─────────────────────────────────────────

test.describe('Inspector API', () => {
  test('GET /_inspector/blocks returns JSON array', async ({ request }) => {
    const resp = await request.get('/_inspector/blocks');
    expect(resp.status()).toBe(200);
    const data = await resp.json();
    expect(Array.isArray(data)).toBeTruthy();
    // Inspector block should be present
    expect(data.some((b: { name: string }) => b.name === '@wafer/inspector')).toBeTruthy();
  });

  test('GET /_inspector/flows returns JSON array', async ({ request }) => {
    const resp = await request.get('/_inspector/flows');
    expect(resp.status()).toBe(200);
    const data = await resp.json();
    expect(Array.isArray(data)).toBeTruthy();
  });

  test('drawflow.js is no longer served as JavaScript', async ({ request }) => {
    const resp = await request.get('/_inspector/drawflow.js');
    // The path falls through to the summary endpoint (returns JSON, not JS)
    const contentType = resp.headers()['content-type'] || '';
    expect(contentType).not.toContain('javascript');
  });

  test('drawflow.css is no longer served as CSS', async ({ request }) => {
    const resp = await request.get('/_inspector/drawflow.css');
    // The path falls through to the summary endpoint (returns JSON, not CSS)
    const contentType = resp.headers()['content-type'] || '';
    expect(contentType).not.toContain('text/css');
  });
});
