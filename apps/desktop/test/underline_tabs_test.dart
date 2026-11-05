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
        tester.getSize(find.byType(UnderlineTab).first).height >= 44,
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
}
