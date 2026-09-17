<?php

namespace App\Events;

class PostPublished
{
    public function __construct(public $post)
    {
    }
}
