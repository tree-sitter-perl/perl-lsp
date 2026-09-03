<?php

namespace App\Providers;

class AppServiceProvider
{
    public function register()
    {
        $this->app->singleton('users.default', fn () => 1);
    }
}
