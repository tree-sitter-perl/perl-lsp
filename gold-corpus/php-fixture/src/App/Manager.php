<?php
namespace App\App;

use App\Base\Manager as BaseManager;

class Manager extends BaseManager
{
    public function __construct(array $x)
    {
        parent::__construct();
        parent::boot('dev', 'extra');
    }
}
