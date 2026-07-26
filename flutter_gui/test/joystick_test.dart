import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:viscous_gui/joystick.dart';
import 'package:viscous_gui/src/rust/api/camera.dart';

void main() {
  Widget wrap(Widget child) => MaterialApp(
    theme: ThemeData(
      colorScheme: ColorScheme.fromSeed(seedColor: Colors.teal),
      useMaterial3: true,
    ),
    home: Scaffold(body: Center(child: child)),
  );

  testWidgets('renders at rest with the puck centered', (tester) async {
    await tester.pumpWidget(wrap(const PanTiltPad(size: 240, onNudge: _noop)));

    await expectLater(
      find.byType(PanTiltPad),
      matchesGoldenFile('goldens/joystick_rest.png'),
    );
  });

  testWidgets(
    'offsets the puck toward a held drag and nudges in that direction',
    (tester) async {
      final calls = <(Direction8, double)>[];
      await tester.pumpWidget(
        wrap(PanTiltPad(size: 240, onNudge: (d, deg) => calls.add((d, deg)))),
      );

      final gesture = await tester.startGesture(
        tester.getCenter(find.byType(PanTiltPad)),
      );
      await gesture.moveBy(const Offset(80, -80)); // up and to the right
      await tester.pump();

      await expectLater(
        find.byType(PanTiltPad),
        matchesGoldenFile('goldens/joystick_dragged_up_right.png'),
      );

      await tester.pump(const Duration(milliseconds: 150));
      expect(calls, isNotEmpty, reason: 'a nudge should fire while held past the deadzone');
      expect(calls.last.$1, Direction8.upRight);

      await gesture.up();
      await tester.pumpAndSettle();

      await expectLater(
        find.byType(PanTiltPad),
        matchesGoldenFile('goldens/joystick_released.png'),
      );
    },
  );

  testWidgets('sends nothing for a drag inside the deadzone', (tester) async {
    final calls = <(Direction8, double)>[];
    await tester.pumpWidget(
      wrap(PanTiltPad(size: 240, onNudge: (d, deg) => calls.add((d, deg)))),
    );

    final gesture = await tester.startGesture(
      tester.getCenter(find.byType(PanTiltPad)),
    );
    await gesture.moveBy(const Offset(2, 2));
    await tester.pump(const Duration(milliseconds: 150));
    await gesture.up();

    expect(calls, isEmpty);
  });
}

void _noop(Direction8 direction, double degrees) {}
