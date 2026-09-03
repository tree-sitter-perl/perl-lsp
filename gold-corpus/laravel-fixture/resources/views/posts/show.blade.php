@extends('layouts.app')
@section('content')
<a href="{{ route('home') }}">{{ __('auth.failed') }}</a>
@can('update', $post)
<a href="{{ route('posts.update', $post) }}">edit</a>
@endcan
@endsection
