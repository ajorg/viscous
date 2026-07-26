import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import 'src/rust/api/camera.dart';

/// Sends a pan/tilt nudge to the connected camera. The default `onNudge` for
/// [PanTiltPad] — split out so widget tests can substitute a spy instead of
/// making a real FFI call with no camera (or native library) present.
void _sendNudgeToCamera(Direction8 direction, double degrees) =>
    nudgePanTilt(direction: direction, degrees: degrees);

/// The pan/tilt drag surface: click-and-drag in any of eight directions,
/// with distance from center controlling nudge size — the same idea as the
/// D70 Commander's jog area, minus its fixed keyboard-era layout.
class PanTiltPad extends StatefulWidget {
  const PanTiltPad({super.key, this.size = 240, this.onNudge = _sendNudgeToCamera});

  final double size;

  /// Called roughly every [_PanTiltPadState._nudgeInterval] while a drag
  /// sits outside the deadzone. Defaults to the real camera bridge call;
  /// tests can substitute their own to observe nudges without one.
  final void Function(Direction8 direction, double degrees) onNudge;

  @override
  State<PanTiltPad> createState() => _PanTiltPadState();
}

class _PanTiltPadState extends State<PanTiltPad>
    with SingleTickerProviderStateMixin {
  /// How often a nudge is sent while the drag sits outside the deadzone.
  static const _nudgeInterval = Duration(milliseconds: 150);

  /// The nudge size, in degrees, at the deadzone edge and at full radius.
  static const _minNudgeDegrees = 1.0;
  static const _maxNudgeDegrees = 10.0;

  /// Drags closer to center than this fraction of the radius send nothing —
  /// otherwise an intended tap-to-recenter would jitter out a tiny nudge.
  static const _deadzoneFraction = 0.12;

  Offset _dragOffset = Offset.zero;
  Timer? _nudgeTimer;
  late final AnimationController _releaseController;
  Animation<Offset>? _releaseAnimation;

  double get _radius => widget.size / 2;
  double get _deadzone => _radius * _deadzoneFraction;

  @override
  void initState() {
    super.initState();
    _releaseController =
        AnimationController(vsync: this, duration: const Duration(milliseconds: 180))
          ..addListener(() {
            final animation = _releaseAnimation;
            if (animation != null) {
              setState(() => _dragOffset = animation.value);
            }
          });
  }

  @override
  void dispose() {
    _nudgeTimer?.cancel();
    _releaseController.dispose();
    super.dispose();
  }

  void _onPanStart(DragStartDetails details) {
    _releaseController.stop();
    _nudgeTimer?.cancel();
    _nudgeTimer = Timer.periodic(_nudgeInterval, (_) => _sendNudge());
    _updateDrag(details.localPosition);
  }

  void _onPanUpdate(DragUpdateDetails details) => _updateDrag(details.localPosition);

  void _onPanEnd(DragEndDetails details) {
    _nudgeTimer?.cancel();
    _nudgeTimer = null;
    _releaseAnimation = Tween<Offset>(begin: _dragOffset, end: Offset.zero).animate(
      CurvedAnimation(parent: _releaseController, curve: Curves.easeOut),
    );
    _releaseController.forward(from: 0);
  }

  void _updateDrag(Offset localPosition) {
    final center = Offset(_radius, _radius);
    var offset = localPosition - center;
    if (offset.distance > _radius) {
      offset = Offset.fromDirection(offset.direction, _radius);
    }
    setState(() => _dragOffset = offset);
  }

  void _sendNudge() {
    final distance = _dragOffset.distance;
    if (distance <= _deadzone) return;

    final t = ((distance - _deadzone) / (_radius - _deadzone)).clamp(0.0, 1.0);
    final degrees = _minNudgeDegrees + t * (_maxNudgeDegrees - _minNudgeDegrees);
    widget.onNudge(_direction8Of(_dragOffset), degrees);
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return GestureDetector(
      onPanStart: _onPanStart,
      onPanUpdate: _onPanUpdate,
      onPanEnd: _onPanEnd,
      onPanCancel: () => _onPanEnd(DragEndDetails()),
      child: CustomPaint(
        size: Size.square(widget.size),
        painter: _PanTiltPainter(
          dragOffset: _dragOffset,
          radius: _radius,
          trackColor: scheme.surfaceContainerHighest,
          accentColor: scheme.primary,
        ),
      ),
    );
  }
}

/// Maps a drag offset to the nearest of eight compass directions.
///
/// Flutter's y axis increases downward, so it's flipped here to make
/// [Direction8.up] mean visually up rather than mathematically up.
Direction8 _direction8Of(Offset offset) {
  final angle = math.atan2(-offset.dy, offset.dx);
  final sector = ((angle / (math.pi / 4)).round() % 8 + 8) % 8;
  const order = [
    Direction8.right,
    Direction8.upRight,
    Direction8.up,
    Direction8.upLeft,
    Direction8.left,
    Direction8.downLeft,
    Direction8.down,
    Direction8.downRight,
  ];
  return order[sector];
}

class _PanTiltPainter extends CustomPainter {
  _PanTiltPainter({
    required this.dragOffset,
    required this.radius,
    required this.trackColor,
    required this.accentColor,
  });

  final Offset dragOffset;
  final double radius;
  final Color trackColor;
  final Color accentColor;

  @override
  void paint(Canvas canvas, Size size) {
    final center = Offset(radius, radius);

    canvas.drawCircle(center, radius, Paint()..color = trackColor);

    final tickPaint = Paint()
      ..color = accentColor.withValues(alpha: 0.25)
      ..strokeWidth = 1;
    for (var i = 0; i < 8; i++) {
      final angle = i * math.pi / 4;
      canvas.drawLine(
        center + Offset.fromDirection(angle, radius * 0.85),
        center + Offset.fromDirection(angle, radius * 0.98),
        tickPaint,
      );
    }

    final puckCenter = center + dragOffset;
    if (dragOffset.distance > 0.5) {
      canvas.drawLine(
        center,
        puckCenter,
        Paint()
          ..color = accentColor.withValues(alpha: 0.4)
          ..strokeWidth = 2,
      );
    }

    canvas.drawCircle(center, 4, Paint()..color = accentColor.withValues(alpha: 0.5));
    canvas.drawCircle(puckCenter, 14, Paint()..color = accentColor);
  }

  @override
  bool shouldRepaint(covariant _PanTiltPainter oldDelegate) =>
      oldDelegate.dragOffset != dragOffset;
}
