import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/underline_tabs.dart';

void main() {
  for (final dark in [false, true]) {
    testWidgets('underline tabs select, wrap and stay flat (dark=$dark)', (
      tester,
    ) async {
      var sel = 'a';
      await tester.pumpWidget(
        MaterialApp(
          theme: dark
              ? ternTheme(TernColors.dark, Brightness.dark)
              : ternTheme(TernColors.light, Brightness.light),
          home: Scaffold(
            body: SizedBox(
              width: 120,
              child: StatefulBuilder(
                builder: (context, setState) => UnderlineTabs(
                  tabs: [
                    for (final k in ['a', 'b', 'c'])
                      UnderlineTab(
                        label: 'Tab $k',
                        selected: sel == k,
                        onTap: () => setState(() => sel = k),
                      ),
                  ],
                ),
              ),
            ),
          ),
        ),
      );
      expect(find.byType(Chip), findsNothing);
      expect(find.byIcon(Icons.check), findsNothing);
      expect(
        tester.getSize(find.byType(UnderlineTab).first).height < 40,
        isTrue,
      );
      // Narrow: wraps onto more than one row.
      expect(
        tester.getTopLeft(find.text('Tab c')).dy >
            tester.getTopLeft(find.text('Tab a')).dy,
        isTrue,
      );
      final handle = tester.ensureSemantics();
      final node = tester.getSemantics(find.text('Tab a'));
      expect(node.flagsCollection.isSelected, isNotNull);
      expect(node.label, 'Tab a');
      handle.dispose();
      await tester.tap(find.text('Tab b'));
      await tester.pump();
      expect(sel, 'b');
    });
  }

  for (final width in [2000.0, 300.0]) {
    testWidgets('tabs lay out in a row, each as wide as its label ($width)', (
      tester,
    ) async {
      tester.view.physicalSize = Size(width + 48, 600);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      const labels = [
        'All',
        'Approvals',
        'Writes',
        'Grants',
        'Outbox',
        'Connections',
        'Connectors',
        'Autonomy',
        'Webhooks',
        'Devices',
      ];
      await tester.pumpWidget(
        MaterialApp(
          theme: ternTheme(TernColors.dark, Brightness.dark),
          home: Scaffold(
            body: ListView(
              padding: const EdgeInsets.symmetric(horizontal: 24),
              children: [
                SizedBox(
                  width: width,
                  child: UnderlineTabs(
                    tabs: [
                      for (final l in labels)
                        UnderlineTab(
                          label: l,
                          selected: l == 'All',
                          onTap: () {},
                        ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      );
      final tabs = find.byType(UnderlineTab);
      final rows = {
        for (final l in labels) tester.getTopLeft(find.text(l)).dy.round(),
      };
      if (width > 1000) {
        expect(rows.length, 1);
      } else {
        expect(rows.length, greaterThan(1));
        expect(rows.length, lessThan(labels.length));
      }
      for (var i = 0; i < labels.length; i++) {
        final tab = tester.getSize(tabs.at(i));
        final label = tester.getSize(find.text(labels[i]));
        expect(tab.width, closeTo(label.width + 4, 0.5), reason: labels[i]);
        expect(tab.height < 40, isTrue);
      }
    });
  }
}
