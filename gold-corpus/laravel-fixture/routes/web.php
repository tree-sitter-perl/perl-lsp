<?php

use App\Http\Controllers\PostController;
use Illuminate\Support\Facades\Route;

Route::get('/', [PostController::class, 'index'])->name('home');
Route::get('/posts/{post}', [PostController::class, 'show'])->name('posts.show');
Route::middleware(['auth'])->group(function () {
    Route::post('/posts/{post}', [PostController::class, 'update'])->name('posts.update');
});
