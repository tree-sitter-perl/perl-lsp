<?php

namespace App\Listeners;

use App\Events\PostPublished;

class NotifyFollowers
{
    public function handle(PostPublished $event)
    {
        return $event->post;
    }
}
