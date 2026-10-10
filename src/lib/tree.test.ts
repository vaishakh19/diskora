import { describe, expect, it } from 'vitest';
import { countNodes, DEFAULT_PRUNE_OPTIONS, isSyntheticPath, otherNodeName, pruneTree, treeDepth, withoutSynthetic } from './tree';
import type { TreeNode } from '@/types/contracts';
import { stablePathId } from '@/lib/id';

function file(path: string, sizeBytes: number, extra: Partial<TreeNode> = {}): TreeNode {
  return {
    id: stablePathId(path),
    path,
    name: path.split('\\').pop() ?? path,
    kind: 'file',
    sizeBytes,
    diskSizeBytes: sizeBytes,
    fileCount: 1,
    folderCount: 0,
    modifiedAt: 1_700_000_000_000,
    createdAt: 1_600_000_000_000,
    extension: null,
    category: 'other',
    accessDenied: false,
    children: [],
    synthetic: false,
    ...extra,
  };
}

function folder(path: string, children: TreeNode[], extra: Partial<TreeNode> = {}): TreeNode {
  return {
    id: stablePathId(path),
    path,
    name: path.split('\\').pop() ?? path,
    kind: 'folder',
    sizeBytes: children.reduce((sum, child) => sum + child.sizeBytes, 0),
    diskSizeBytes: children.reduce((sum, child) => sum + (child.diskSizeBytes ?? 0), 0),
    fileCount: children.reduce((sum, child) => sum + child.fileCount, 0),
    folderCount: children.reduce((sum, child) => sum + child.folderCount + (child.kind === 'folder' ? 1 : 0), 0),
    modifiedAt: 1_700_000_000_000,
    createdAt: 1_600_000_000_000,
    extension: null,
    category: 'other',
    accessDenied: false,
    children,
    synthetic: false,
    ...extra,
  };
}

/** A tree with `width` children per level and `depth` levels. */
function makeTree(width: number, depth: number): TreeNode {
  const build = (path: string, level: number): TreeNode => {
    if (level === 0) return file(`${path}\\file.bin`, 1000);
    const children: TreeNode[] = [];
    for (let i = 0; i < width; i += 1) children.push(build(`${path}\\child-${i}`, level - 1));
    return folder(path, children);
  };
  return build('C:\\root', depth);
}

describe('pruneTree', () => {
  it('keeps small trees untouched', () => {
    const tree = makeTree(3, 2);
    const result = pruneTree(tree);
    expect(result.prunedNodes).toBe(0);
    expect(result.truncated).toBe(false);
    expect(countNodes(result.tree)).toBe(countNodes(tree));
    expect(result.tree.sizeBytes).toBe(tree.sizeBytes);
  });

  it('limits children per node and folds the remainder into one honest aggregate', () => {
    const children = Array.from({ length: 250 }, (_, i) => file(`C:\\root\\f-${i}`, (i + 1) * 1000));
    const tree = folder('C:\\root', children);
    const result = pruneTree(tree, { maxChildren: 100, maxNodes: 100_000 });

    expect(result.tree.children).toHaveLength(100);
    expect(result.tree.children[99].synthetic).toBe(true);
    expect(result.tree.children[99].name).toBe('Other (151 items)');
    // Sizes stay exact.
    expect(result.tree.sizeBytes).toBe(tree.sizeBytes);
    expect(result.tree.children.reduce((sum, child) => sum + child.sizeBytes, 0)).toBe(tree.sizeBytes);
    // The biggest consumers are kept, in descending order.
    expect(result.tree.children[0].sizeBytes).toBe(250_000);
    expect(result.tree.children[98].sizeBytes).toBe(152_000);
    expect(result.prunedNodes).toBe(151);
  });

  it('enforces a global node budget by limiting depth', () => {
    const tree = makeTree(10, 5); // 1 + 10 + 100 + 1000 + 10000 + 100000 nodes
    const result = pruneTree(tree, { maxChildren: 100, maxNodes: 2000 });
    expect(countNodes(result.tree)).toBeLessThanOrEqual(2000);
    expect(treeDepth(result.tree)).toBeLessThan(5);
    // The root's total is still exact.
    expect(result.tree.sizeBytes).toBe(tree.sizeBytes);
  });

  it('never produces a tree larger than the budget', () => {
    const tree = makeTree(6, 6);
    for (const maxNodes of [50, 500, 5000, 50_000]) {
      const result = pruneTree(tree, { maxChildren: 20, maxNodes });
      expect(countNodes(result.tree)).toBeLessThanOrEqual(maxNodes);
    }
  });

  it('uses the documented defaults', () => {
    expect(DEFAULT_PRUNE_OPTIONS.maxChildren).toBe(100);
    expect(DEFAULT_PRUNE_OPTIONS.maxNodes).toBe(150_000);
  });

  it('treats a maxChildren below 2 as 2', () => {
    const tree = folder('C:\\root', [file('C:\\root\\a', 1), file('C:\\root\\b', 2), file('C:\\root\\c', 3)]);
    const result = pruneTree(tree, { maxChildren: 0, maxNodes: 100_000 });
    expect(result.tree.children.length).toBeLessThanOrEqual(3);
  });
});

describe('helpers', () => {
  it('names the aggregate node', () => {
    expect(otherNodeName(1)).toBe('Other (1 item)');
    expect(otherNodeName(151)).toBe('Other (151 items)');
  });

  it('detects synthetic paths', () => {
    expect(isSyntheticPath('C:\\root\\[Other]')).toBe(true);
    expect(isSyntheticPath('C:\\root\\real')).toBe(false);
  });

  it('filters synthetic nodes out of listings', () => {
    const tree = folder('C:\\root', [file('C:\\root\\a', 1)]);
    const pruned = pruneTree(tree);
    pruned.tree.children.push({
      ...file('C:\\root\\[Other]', 5),
      name: otherNodeName(1),
      synthetic: true,
      kind: 'folder',
    });
    expect(withoutSynthetic(pruned.tree.children)).toHaveLength(1);
  });

  it('computes depth', () => {
    expect(treeDepth(file('C:\\a', 1))).toBe(0);
    expect(treeDepth(makeTree(2, 3))).toBe(3);
  });
});
