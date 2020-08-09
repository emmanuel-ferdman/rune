use crate::collections::HashMap;
use crate::future::Future;
use crate::hash::Hash;
use crate::reflection::{ReflectValueType, ToValue, UnsafeFromValue};
use crate::tls;
use crate::value::{ValueType, ValueTypeInfo};
use crate::vm::{Vm, VmError};
use std::any::type_name;
use std::future;

use crate::context::item::Item;
use crate::context::{ContextError, Handler, IntoInstFnHash};

/// A tuple variant.
pub(super) struct TupleVariant {
    /// Full name of the variant.
    pub(super) name: Item,
    /// Function to use when testing if variant is a tuple that can be matched
    /// over.
    pub(super) tuple_match: Box<Handler>,
    /// Function to use when constructing a tuple.
    pub(super) tuple_constructor: Box<Handler>,
    /// The value type of the type.
    pub(super) value_type: ValueType,
    /// Information on the type.
    pub(super) value_type_info: ValueTypeInfo,
    /// The number of arguments the meta argument has.
    pub(super) args: usize,
}

pub(super) enum Variant {
    TupleVariant(TupleVariant),
}

pub(super) struct Type {
    pub(super) name: Item,
    pub(super) value_type_info: ValueTypeInfo,
}

pub struct InstanceFunction {
    pub(super) handler: Box<Handler>,
    pub(super) args: Option<usize>,
    pub(super) value_type_info: ValueTypeInfo,
    pub(super) name: String,
}

/// A collection of functions that can be looked up by type.
#[derive(Default)]
pub struct Module {
    /// The name of the module.
    pub(super) path: Item,
    /// Free functions.
    pub(super) functions: HashMap<Item, (Box<Handler>, Option<usize>)>,
    /// Instance functions.
    pub(super) instance_functions: HashMap<(ValueType, Hash), InstanceFunction>,
    /// Registered types.
    pub(super) types: HashMap<ValueType, Type>,
    /// Registered variants.
    pub(super) variants: Vec<Variant>,
}

impl Module {
    /// Construct a new module.
    pub fn new<I>(path: I) -> Self
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        Self {
            path: Item::of(path),
            functions: Default::default(),
            instance_functions: Default::default(),
            types: Default::default(),
            variants: Default::default(),
        }
    }

    /// Register a type.
    ///
    /// This will allow the type to be used within scripts, using the item named
    /// here.
    pub fn ty<N>(&mut self, name: N) -> TypeBuilder<'_, N>
    where
        N: IntoIterator,
        N::Item: AsRef<str>,
    {
        TypeBuilder {
            name,
            types: &mut self.types,
        }
    }

    /// Register a variant.
    ///
    /// This will allow the type to be used within scripts, using the item named
    /// here.
    pub fn variant<N>(&mut self, name: N) -> VariantBuilder<'_, N>
    where
        N: IntoIterator,
        N::Item: AsRef<str>,
    {
        VariantBuilder {
            name,
            variants: &mut self.variants,
        }
    }

    /// Register a function that cannot error internally.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::collections::VecDeque;
    ///
    /// #[derive(Debug, Clone)]
    /// struct StringQueue(VecDeque<String>);
    ///
    /// impl StringQueue {
    ///     fn new() -> Self {
    ///         Self(VecDeque::new())
    ///     }
    /// }
    ///
    /// runestick::decl_external!(StringQueue);
    ///
    /// # fn main() -> runestick::Result<()> {
    /// let mut module = runestick::Module::default();
    ///
    /// module.function(&["bytes"], StringQueue::new)?;
    /// module.function(&["empty"], || Ok::<_, runestick::Error>(()))?;
    /// module.function(&["string"], |a: String| Ok::<_, runestick::Error>(()))?;
    /// module.function(&["optional"], |a: Option<String>| Ok::<_, runestick::Error>(()))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn function<Func, Args, N>(&mut self, name: N, f: Func) -> Result<(), ContextError>
    where
        Func: Function<Args>,
        N: IntoIterator,
        N::Item: AsRef<str>,
    {
        let name = Item::of(name);

        if self.functions.contains_key(&name) {
            return Err(ContextError::ConflictingFunctionName { name });
        }

        let handler: Box<Handler> = Box::new(move |vm, args| f.vm_call(vm, args));
        self.functions.insert(name, (handler, Some(Func::args())));
        Ok(())
    }

    /// Register a function.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # fn main() -> runestick::Result<()> {
    /// let mut module = runestick::Module::default();
    ///
    /// module.async_function(&["empty"], || async { () })?;
    /// module.async_function(&["empty_fallible"], || async { Ok::<_, runestick::Error>(()) })?;
    /// module.async_function(&["string"], |a: String| async { Ok::<_, runestick::Error>(()) })?;
    /// module.async_function(&["optional"], |a: Option<String>| async { Ok::<_, runestick::Error>(()) })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn async_function<Func, Args, N>(&mut self, name: N, f: Func) -> Result<(), ContextError>
    where
        Func: AsyncFunction<Args>,
        N: IntoIterator,
        N::Item: AsRef<str>,
    {
        let name = Item::of(name);

        if self.functions.contains_key(&name) {
            return Err(ContextError::ConflictingFunctionName { name });
        }

        let handler: Box<Handler> = Box::new(move |vm, args| f.vm_call(vm, args));
        self.functions.insert(name, (handler, Some(Func::args())));
        Ok(())
    }

    /// Register a raw function which interacts directly with the virtual
    /// machine.
    pub fn raw_fn<F, N>(&mut self, name: N, f: F) -> Result<(), ContextError>
    where
        for<'vm> F: 'static + Copy + Fn(&'vm mut Vm, usize) -> Result<(), VmError> + Send + Sync,
        N: IntoIterator,
        N::Item: AsRef<str>,
    {
        let name = Item::of(name);

        if self.functions.contains_key(&name) {
            return Err(ContextError::ConflictingFunctionName { name });
        }

        let handler: Box<Handler> = Box::new(move |vm, args| f(vm, args));
        self.functions.insert(name, (handler, None));
        Ok(())
    }

    /// Register an instance function.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::collections::VecDeque;
    ///
    /// #[derive(Debug, Clone)]
    /// struct StringQueue(VecDeque<String>);
    ///
    /// impl StringQueue {
    ///     fn new() -> Self {
    ///         Self(VecDeque::new())
    ///     }
    ///
    ///     fn len(&self) -> usize {
    ///         self.0.len()
    ///     }
    /// }
    ///
    /// runestick::decl_external!(StringQueue);
    ///
    /// # fn main() -> runestick::Result<()> {
    /// let mut module = runestick::Module::default();
    ///
    /// module.ty(&["StringQueue"]).build::<StringQueue>()?;
    /// module.function(&["StringQueue", "bytes"], StringQueue::new)?;
    /// module.inst_fn("len", StringQueue::len)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn inst_fn<N, Func, Args>(&mut self, name: N, f: Func) -> Result<(), ContextError>
    where
        N: IntoInstFnHash,
        Func: InstFn<Args>,
    {
        let ty = Func::instance_value_type();
        let value_type_info = Func::instance_value_type_info();

        let key = (ty, name.to_hash());
        let name = name.to_name();

        if self.instance_functions.contains_key(&key) {
            return Err(ContextError::ConflictingInstanceFunction {
                value_type_info,
                name,
            });
        }

        let handler: Box<Handler> = Box::new(move |vm, args| f.vm_call(vm, args));

        let instance_function = InstanceFunction {
            handler,
            args: Some(Func::args()),
            value_type_info,
            name,
        };

        self.instance_functions.insert(key, instance_function);

        Ok(())
    }

    /// Register an instance function.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::atomic::AtomicU32;
    /// use std::sync::Arc;
    ///
    /// runestick::decl_external!(MyType);
    ///
    /// #[derive(Clone, Debug)]
    /// struct MyType {
    ///     value: Arc<AtomicU32>,
    /// }
    ///
    /// impl MyType {
    ///     async fn test(&self) -> runestick::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// # fn main() -> runestick::Result<()> {
    /// let mut module = runestick::Module::default();
    ///
    /// module.ty(&["MyType"]).build::<MyType>()?;
    /// module.async_inst_fn("test", MyType::test)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn async_inst_fn<N, Func, Args>(&mut self, name: N, f: Func) -> Result<(), ContextError>
    where
        N: IntoInstFnHash,
        Func: AsyncInstFn<Args>,
    {
        let ty = Func::instance_value_type();
        let value_type_info = Func::instance_value_type_info();

        let key = (ty, name.to_hash());
        let name = name.to_name();

        if self.instance_functions.contains_key(&key) {
            return Err(ContextError::ConflictingInstanceFunction {
                value_type_info,
                name,
            });
        }

        let handler: Box<Handler> = Box::new(move |vm, args| f.vm_call(vm, args));

        let instance_function = InstanceFunction {
            handler,
            args: Some(Func::args()),
            value_type_info,
            name,
        };

        self.instance_functions.insert(key, instance_function);
        Ok(())
    }
}

/// The builder for a type.
#[must_use = "must be consumed with build::<T>() to construct a type"]
pub struct TypeBuilder<'a, N> {
    name: N,
    types: &'a mut HashMap<ValueType, Type>,
}

impl<N> TypeBuilder<'_, N>
where
    N: IntoIterator,
    N::Item: AsRef<str>,
{
    /// Construct a new type, specifying which type it is with the parameter.
    pub fn build<T>(self) -> Result<(), ContextError>
    where
        T: ReflectValueType,
    {
        let name = Item::of(self.name);
        let value_type = T::value_type();
        let value_type_info = T::value_type_info();

        let ty = Type {
            name: name.clone(),
            value_type_info,
        };

        if let Some(old) = self.types.insert(value_type, ty) {
            return Err(ContextError::ConflictingType {
                name,
                existing: old.value_type_info,
            });
        }

        Ok(())
    }
}

/// The builder for a type.
#[must_use = "must be consumed with build::<T>() to construct a variant"]
pub struct VariantBuilder<'a, N> {
    name: N,
    variants: &'a mut Vec<Variant>,
}

impl<N> VariantBuilder<'_, N>
where
    N: IntoIterator,
    N::Item: AsRef<str>,
{
    /// Perform a tuple match.
    pub fn tuple<Match, MatchArgs, Constructor, ConstructorArgs>(
        self,
        tuple_match: Match,
        tuple_constructor: Constructor,
    ) where
        Match: InstFn<MatchArgs>,
        Constructor: Function<ConstructorArgs>,
    {
        let name = Item::of(self.name);
        let tuple_match: Box<Handler> = Box::new(move |vm, args| tuple_match.vm_call(vm, args));
        let tuple_constructor: Box<Handler> =
            Box::new(move |vm, args| tuple_constructor.vm_call(vm, args));
        let value_type = Match::instance_value_type();
        let value_type_info = Match::instance_value_type_info();

        self.variants.push(Variant::TupleVariant(TupleVariant {
            name,
            tuple_match,
            tuple_constructor,
            value_type,
            value_type_info,
            args: Constructor::args(),
        }));
    }
}

/// Helper trait to convert function return values into results.
pub trait IntoVmResult {
    type Output: ToValue;

    fn into_vm_result(self) -> Result<Self::Output, VmError>;
}

impl<T> IntoVmResult for T
where
    T: ToValue,
{
    type Output = T;

    fn into_vm_result(self) -> Result<Self::Output, VmError> {
        Ok(self)
    }
}

/// Trait used to provide the [function][Context::function] function.
pub trait Function<Args>: 'static + Copy + Send + Sync {
    /// Get the number of arguments.
    fn args() -> usize;

    /// Perform the vm call.
    fn vm_call(self, vm: &mut Vm, args: usize) -> Result<(), VmError>;
}

/// Trait used to provide the [async_function][Context::async_function] function.
pub trait AsyncFunction<Args>: 'static + Copy + Send + Sync {
    /// Get the number of arguments.
    fn args() -> usize;

    /// Perform the vm call.
    fn vm_call(self, vm: &mut Vm, args: usize) -> Result<(), VmError>;
}

/// Trait used to provide the [inst_fn][Context::inst_fn] function.
pub trait InstFn<Args>: 'static + Copy + Send + Sync {
    /// Get the number of arguments.
    fn args() -> usize;

    /// Access the value type of the instance.
    fn instance_value_type() -> ValueType;

    /// Access the value type info of the instance.
    fn instance_value_type_info() -> ValueTypeInfo;

    /// Perform the vm call.
    fn vm_call(self, vm: &mut Vm, args: usize) -> Result<(), VmError>;
}

/// Trait used to provide the [async_inst_fn][Context::async_inst_fn] function.
pub trait AsyncInstFn<Args>: 'static + Copy + Send + Sync {
    /// Get the number of arguments.
    fn args() -> usize;

    /// Access the value type of the instance.
    fn instance_value_type() -> ValueType;

    /// Access the value type of the instance.
    fn instance_value_type_info() -> ValueTypeInfo;

    /// Perform the vm call.
    fn vm_call(self, vm: &mut Vm, args: usize) -> Result<(), VmError>;
}

macro_rules! impl_register {
    () => {
        impl_register!{@impl 0,}
    };

    ({$ty:ident, $var:ident, $num:expr}, $({$l_ty:ident, $l_var:ident, $l_num:expr},)*) => {
        impl_register!{@impl $num, {$ty, $var, $num}, $({$l_ty, $l_var, $l_num},)*}
        impl_register!{$({$l_ty, $l_var, $l_num},)*}
    };

    (@impl $count:expr, $({$ty:ident, $var:ident, $num:expr},)*) => {
        impl<Func, Ret, $($ty,)*> Function<($($ty,)*)> for Func
        where
            Func: 'static + Copy + Send + Sync + Fn($($ty,)*) -> Ret,
            Ret: IntoVmResult,
            Ret::Output: ToValue,
            $($ty: UnsafeFromValue,)*
        {
            fn args() -> usize {
                $count
            }

            fn vm_call(
                self,
                vm: &mut Vm,
                args: usize
            ) -> Result<(), VmError> {
                impl_register!{@check-args $count, args}
                $(let $var = vm.pop()?;)*

                // Safety: We hold a reference to the Vm, so we can
                // guarantee that it won't be modified.
                //
                // The scope is also necessary, since we mutably access `vm`
                // when we return below.
                #[allow(unused)]
                let ret = unsafe {
                    impl_register!{@unsafe-vars vm, $count, $($ty, $var, $num,)*}
                    tls::inject_vm(vm, || self($(<$ty>::to_arg($var.0),)*)).into_vm_result()?
                };

                impl_register!{@return vm, ret, Ret}
                Ok(())
            }
        }

        impl<Func, Ret, $($ty,)*> AsyncFunction<($($ty,)*)> for Func
        where
            Func: 'static + Copy + Send + Sync + Fn($($ty,)*) -> Ret,
            Ret: future::Future,
            Ret::Output: IntoVmResult,
            $($ty: 'static + UnsafeFromValue,)*
        {
            fn args() -> usize {
                $count
            }

            fn vm_call(
                self,
                vm: &mut Vm,
                args: usize
            ) -> Result<(), VmError> {
                impl_register!{@check-args $count, args}
                $(let $var = vm.pop()?;)*

                // Safety: Future is owned and will only be called within the
                // context of the virtual machine, which will provide
                // exclusive thread-local access to itself while the future is
                // being polled.
                let ret = unsafe {
                    let future: Box<dyn future::Future<Output = Result<(), VmError>>> = Box::new(async move {
                        #[allow(unused)]
                        let ($($var,)*) = tls::with_vm(|vm| {
                            impl_register!{@unsafe-vars vm, $count, $($ty, $var, $num,)*}
                            Ok::<_, VmError>(($($var,)*))
                        })?;

                        let output = self($(<$ty>::to_arg($var.0),)*).await.into_vm_result()?;

                        tls::with_vm(|vm| {
                            let value = output.to_value(vm)?;
                            vm.push(value);
                            Ok::<_, VmError>(())
                        })?;

                        Ok(())
                    });

                    Future::new_unchecked(Box::into_raw(future))
                };

                impl_register!{@return vm, ret, Ret}
                Ok(())
            }
        }

        impl<Func, Ret, Inst, $($ty,)*> InstFn<(Inst, $($ty,)*)> for Func
        where
            Func: 'static + Copy + Send + Sync + Fn(Inst $(, $ty)*) -> Ret,
            Ret: IntoVmResult,
            Inst: UnsafeFromValue + ReflectValueType,
            $($ty: UnsafeFromValue,)*
        {
            fn args() -> usize {
                $count
            }

            fn instance_value_type() -> ValueType {
                Inst::value_type()
            }

            fn instance_value_type_info() -> ValueTypeInfo {
                Inst::value_type_info()
            }

            fn vm_call(self, vm: &mut Vm, args: usize) -> Result<(), VmError> {
                impl_register!{@check-args $count, args}
                let inst = vm.pop()?;
                $(let $var = vm.pop()?;)*

                // Safety: We hold a reference to the Vm, so we can
                // guarantee that it won't be modified.
                //
                // The scope is also necessary, since we mutably access `vm`
                // when we return below.
                #[allow(unused)]
                let ret = unsafe {
                    impl_register!{@unsafe-inst-vars inst, vm, $count, $($ty, $var, $num,)*}
                    tls::inject_vm(vm, || self(Inst::to_arg(inst.0), $(<$ty>::to_arg($var.0),)*)).into_vm_result()?
                };

                impl_register!{@return vm, ret, Ret}
                Ok(())
            }
        }

        impl<Func, Ret, Inst, $($ty,)*> AsyncInstFn<(Inst, $($ty,)*)> for Func
        where
            Func: 'static + Copy + Send + Sync + Fn(Inst $(, $ty)*) -> Ret,
            Ret: future::Future,
            Ret::Output: IntoVmResult,
            Inst: UnsafeFromValue + ReflectValueType,
            $($ty: UnsafeFromValue,)*
        {
            fn args() -> usize {
                $count
            }

            fn instance_value_type() -> ValueType {
                Inst::value_type()
            }

            fn instance_value_type_info() -> ValueTypeInfo {
                Inst::value_type_info()
            }

            fn vm_call(self, vm: &mut Vm, args: usize) -> Result<(), VmError> {
                impl_register!{@check-args $count, args}
                let inst = vm.pop()?;
                $(let $var = vm.pop()?;)*

                // Safety: Future is owned and will only be called within the
                // context of the virtual machine, which will provide
                // exclusive thread-local access to itself while the future is
                // being polled.
                #[allow(unused)]
                let ret = unsafe {
                    let future: Box<dyn future::Future<Output = Result<(), VmError>>> = Box::new(async move {
                        let (inst, $($var,)*) = tls::with_vm(|vm| {
                            impl_register!{@unsafe-inst-vars inst, vm, $count, $($ty, $var, $num,)*}
                            Ok((inst, $($var,)*))
                        })?;

                        let output = self(Inst::to_arg(inst.0), $(<$ty>::to_arg($var.0),)*).await.into_vm_result()?;

                        tls::with_vm(|vm| {
                            let value = output.to_value(vm)?;
                            vm.push(value);
                            Ok::<_, VmError>(())
                        })?;

                        Ok(())
                    });

                    Future::new_unchecked(Box::into_raw(future))
                };

                impl_register!{@return vm, ret, Ret}
                Ok(())
            }
        }
    };

    (@return $vm:ident, $ret:ident, $ty:ty) => {
        let $ret = match $ret.to_value($vm) {
            Ok($ret) => $ret,
            Err(error) => {
                return Err(VmError::ReturnConversionError {
                    error: Box::new(error),
                    ret: type_name::<$ty>()
                });
            }
        };

        $vm.push($ret);
    };

    // Expand to function variable bindings.
    (@unsafe-vars $vm:expr, $count:expr, $($ty:ty, $var:ident, $num:expr,)*) => {
        $(
            let $var = match <$ty>::unsafe_from_value($var, $vm) {
                Ok(v) => v,
                Err(error) => {
                    let ty = $var.type_info($vm)?;

                    return Err(VmError::ArgumentConversionError {
                        error: Box::new(error),
                        arg: $count - $num,
                        from: ty,
                        to: type_name::<$ty>(),
                    });
                }
            };
        )*
    };

    // Expand to instance variable bindings.
    (@unsafe-inst-vars $inst:ident, $vm:expr, $count:expr, $($ty:ty, $var:ident, $num:expr,)*) => {
        let $inst = match Inst::unsafe_from_value($inst, $vm) {
            Ok(v) => v,
            Err(error) => {
                let ty = $inst.type_info($vm)?;

                return Err(VmError::ArgumentConversionError {
                    error: Box::new(error),
                    arg: 0,
                    from: ty,
                    to: type_name::<Inst>()
                });
            }
        };

        $(
            let $var = match <$ty>::unsafe_from_value($var, $vm) {
                Ok(v) => v,
                Err(error) => {
                    let ty = $var.type_info($vm)?;

                    return Err(VmError::ArgumentConversionError {
                        error: Box::new(error),
                        arg: 1 + $count - $num,
                        from: ty,
                        to: type_name::<$ty>()
                    });
                }
            };
        )*
    };

    (@check-args $expected:expr, $actual:expr) => {
        if $actual != $expected {
            return Err(VmError::ArgumentCountMismatch {
                actual: $actual,
                expected: $expected,
            });
        }
    };
}

impl_register!(
    {H, h, 8},
    {G, g, 7},
    {F, f, 6},
    {E, e, 5},
    {D, d, 4},
    {C, c, 3},
    {B, b, 2},
    {A, a, 1},
);