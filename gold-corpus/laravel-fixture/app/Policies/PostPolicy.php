<?php

namespace App\Policies;

class PostPolicy
{
    public function update($user, $post)
    {
        return true;
    }
}
