import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:viscous_gui/camera_screen.dart';

void main() {
  testWidgets('shows the serial port form before connecting', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: CameraScreen()));

    expect(find.widgetWithText(TextFormField, 'Serial port'), findsOneWidget);
    expect(find.text('Connect'), findsOneWidget);
  });

  testWidgets('requires a non-empty port before connecting', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: CameraScreen()));

    await tester.enterText(find.byType(TextFormField), '');
    await tester.tap(find.text('Connect'));
    await tester.pump();

    expect(find.text('Required'), findsOneWidget);
  });

  testWidgets('matches the connect form golden', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: CameraScreen()));

    await expectLater(
      find.byType(CameraScreen),
      matchesGoldenFile('goldens/connect_form.png'),
    );
  });
}
