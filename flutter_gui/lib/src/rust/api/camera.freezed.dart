// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'camera.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$StatusEvent {

 Object get field0;



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is StatusEvent&&const DeepCollectionEquality().equals(other.field0, field0));
}


@override
int get hashCode => Object.hash(runtimeType,const DeepCollectionEquality().hash(field0));

@override
String toString() {
  return 'StatusEvent(field0: $field0)';
}


}

/// @nodoc
class $StatusEventCopyWith<$Res>  {
$StatusEventCopyWith(StatusEvent _, $Res Function(StatusEvent) __);
}


/// Adds pattern-matching-related methods to [StatusEvent].
extension StatusEventPatterns on StatusEvent {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( StatusEvent_State value)?  state,TResult Function( StatusEvent_Command value)?  command,required TResult orElse(),}){
final _that = this;
switch (_that) {
case StatusEvent_State() when state != null:
return state(_that);case StatusEvent_Command() when command != null:
return command(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( StatusEvent_State value)  state,required TResult Function( StatusEvent_Command value)  command,}){
final _that = this;
switch (_that) {
case StatusEvent_State():
return state(_that);case StatusEvent_Command():
return command(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( StatusEvent_State value)?  state,TResult? Function( StatusEvent_Command value)?  command,}){
final _that = this;
switch (_that) {
case StatusEvent_State() when state != null:
return state(_that);case StatusEvent_Command() when command != null:
return command(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( CameraState field0)?  state,TResult Function( String field0)?  command,required TResult orElse(),}) {final _that = this;
switch (_that) {
case StatusEvent_State() when state != null:
return state(_that.field0);case StatusEvent_Command() when command != null:
return command(_that.field0);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( CameraState field0)  state,required TResult Function( String field0)  command,}) {final _that = this;
switch (_that) {
case StatusEvent_State():
return state(_that.field0);case StatusEvent_Command():
return command(_that.field0);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( CameraState field0)?  state,TResult? Function( String field0)?  command,}) {final _that = this;
switch (_that) {
case StatusEvent_State() when state != null:
return state(_that.field0);case StatusEvent_Command() when command != null:
return command(_that.field0);case _:
  return null;

}
}

}

/// @nodoc


class StatusEvent_State extends StatusEvent {
  const StatusEvent_State(this.field0): super._();
  

@override final  CameraState field0;

/// Create a copy of StatusEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$StatusEvent_StateCopyWith<StatusEvent_State> get copyWith => _$StatusEvent_StateCopyWithImpl<StatusEvent_State>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is StatusEvent_State&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'StatusEvent.state(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $StatusEvent_StateCopyWith<$Res> implements $StatusEventCopyWith<$Res> {
  factory $StatusEvent_StateCopyWith(StatusEvent_State value, $Res Function(StatusEvent_State) _then) = _$StatusEvent_StateCopyWithImpl;
@useResult
$Res call({
 CameraState field0
});




}
/// @nodoc
class _$StatusEvent_StateCopyWithImpl<$Res>
    implements $StatusEvent_StateCopyWith<$Res> {
  _$StatusEvent_StateCopyWithImpl(this._self, this._then);

  final StatusEvent_State _self;
  final $Res Function(StatusEvent_State) _then;

/// Create a copy of StatusEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(StatusEvent_State(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as CameraState,
  ));
}


}

/// @nodoc


class StatusEvent_Command extends StatusEvent {
  const StatusEvent_Command(this.field0): super._();
  

@override final  String field0;

/// Create a copy of StatusEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$StatusEvent_CommandCopyWith<StatusEvent_Command> get copyWith => _$StatusEvent_CommandCopyWithImpl<StatusEvent_Command>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is StatusEvent_Command&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'StatusEvent.command(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $StatusEvent_CommandCopyWith<$Res> implements $StatusEventCopyWith<$Res> {
  factory $StatusEvent_CommandCopyWith(StatusEvent_Command value, $Res Function(StatusEvent_Command) _then) = _$StatusEvent_CommandCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$StatusEvent_CommandCopyWithImpl<$Res>
    implements $StatusEvent_CommandCopyWith<$Res> {
  _$StatusEvent_CommandCopyWithImpl(this._self, this._then);

  final StatusEvent_Command _self;
  final $Res Function(StatusEvent_Command) _then;

/// Create a copy of StatusEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(StatusEvent_Command(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

// dart format on
