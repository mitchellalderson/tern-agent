import 'dart:convert';
import 'dart:io';

import 'package:tern_client/tern_client.dart';
import 'package:test/test.dart';

/// The fixtures are real API output: `serde_json` of a tern-bundle plan
/// for an update that adds a lookalike host, and of the installed list.
Json _fixture(String name) =>
    jsonDecode(File('test/fixtures/$name').readAsStringSync()) as Json;

void main() {
  test('decodes a preview: the capability diff and the lookalike', () {
    final p = BundlePreview.fromJson(_fixture('bundle_preview.json'));
    expect(p.canInstall, isTrue);
    expect(p.grows, isTrue);
    final plan = p.plan;
    expect(plan.name, 'notion-recipe');
    expect(plan.isUpdate, isTrue);
    expect(plan.installedVersion, '1.0.0');
    expect(plan.signatureState, 'pinned');
    expect(plan.signatureWords, contains('the key of the installed version'));
    expect(plan.added, ['connector notion: reaches notion-api.co']);

    final notion = plan.connectors.firstWhere((c) => c.name == 'notion');
    expect(notion.hosts, contains('notion-api.co'));
    expect(notion.hasLookalike, isTrue);
    expect(notion.rate, '60/min');
    expect(notion.tools.map((t) => (t.name, t.effect)), [
      ('create_page', 'write'),
      ('search', 'read'),
    ]);
    final gmail = plan.connectors.firstWhere((c) => c.name == 'gmail');
    expect(gmail.hasLookalike, isFalse);
    expect(gmail.oauth, 'google (scopes: gmail.readonly)');

    expect(plan.secrets.single.name, 'NOTION_TOKEN');
    expect(plan.secrets.single.set, isFalse);
    expect(plan.connections.single.provider, 'google');
    expect(plan.routines.map((r) => r.trigger), [
      'cron 0 8 * * 1-5',
      'watch notion.changed every 15m',
    ]);
    expect(plan.skills.single.slug, 'notion-brief');
    expect(plan.warnings.any((w) => w.contains('notion-api.co')), isTrue);
  });

  test('decodes the agents a bundle brings, with their capability lines', () {
    final p = BundlePreview.fromJson(_fixture('bundle_preview_agents.json'));
    expect(p.canInstall, isFalse);
    expect(p.conflicts.single, contains('@scout'));
    final scout = p.plan.agents.firstWhere((a) => a.slug == 'scout');
    expect(scout.name, 'Scout');
    expect(scout.existing, isTrue);
    expect(scout.allBuiltins, isTrue);
    expect(scout.allConnectors, isTrue);
    expect(scout.allMcp, isFalse);
    expect(scout.sharedMemory, isTrue);
    expect(scout.autonomy, 'autonomous');
    expect(scout.scope('skills'), 'all');
    final reader = p.plan.agents.last;
    expect(reader.kept, isTrue);
    expect(reader.autonomy, isNull);
    expect(reader.allBuiltins, isFalse);
    expect(reader.scope('builtins'), 'read_only');
    expect(p.plan.added, contains('agent @scout: connectors: all'));
    // A plan from a core without agents has none.
    expect(
      BundlePreview.fromJson(_fixture('bundle_preview.json')).plan.agents,
      isEmpty,
    );
  });

  test('the installed list names the agents a bundle made', () {
    final b = InstalledBundle.fromJson({
      'name': 'x',
      'version': '1',
      'source': 's',
      'sha256': 'h',
      'signature': 'unsigned',
      'installed_at': 'now',
      'agents': [
        {'slug': 'scout', 'id': 'tern:scout'},
      ],
    });
    expect(b.agents, ['scout']);
  });

  test('decodes the installed list', () {
    final list = [
      for (final b in _fixture('bundle_list.json')['bundles'] as List)
        InstalledBundle.fromJson(b as Json),
    ];
    final b = list.single;
    expect(b.name, 'notion-recipe');
    expect(b.signed, isTrue);
    expect(b.signature, startsWith('signed by '));
    expect(b.connectors, ['gmail', 'notion']);
    expect(b.routines, ['morning', 'on-change']);
    expect(b.imported, isFalse);
  });
}
