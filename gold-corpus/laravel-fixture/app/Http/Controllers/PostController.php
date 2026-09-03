<?php

namespace App\Http\Controllers;

use App\Events\PostPublished;
use App\PostRepo;

class PostController
{
    public function __construct()
    {
        $this->middleware('auth:web');
    }

    public function index()
    {
        return view('posts.index', ['title' => config('app.name')]);
    }

    public function show($post)
    {
        $this->authorize('update', $post);
        event(new PostPublished($post));
        return redirect()->route('posts.show', $post);
    }

    public function update($post)
    {
        $repo = app(PostRepo::class)->find($post);
        $default = app('users.default');
        return route('nowhere') . __('auth.failed') . view('posts.missing');
    }
}
