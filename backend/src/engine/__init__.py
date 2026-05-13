"""Engine package facade.

Public symbols live on ``engine.api`` (see that module for the lazy
``__getattr__`` re-export surface). The top-level ``from engine import X``
shorthand is not used anywhere in the codebase, so this module only
exists to mark the directory as a package.
"""
