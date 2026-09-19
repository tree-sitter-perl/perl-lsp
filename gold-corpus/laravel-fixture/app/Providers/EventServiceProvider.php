<?php

namespace App\Providers;

use App\Events\PostPublished;
use App\Listeners\NotifyFollowers;

class EventServiceProvider
{
    protected $listen = [
        PostPublished::class => [
            NotifyFollowers::class,
        ],
    ];
}
