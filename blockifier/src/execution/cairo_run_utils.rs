use std::any::Any;
use std::collections::HashMap;

use anyhow::{bail, Result};
use cairo_rs::bigint;
use cairo_rs::hint_processor::hint_processor_definition::HintProcessor;
use cairo_rs::serde::deserialize_program::{
    deserialize_array_of_bigint_hex, deserialize_bigint_hex, HintParams, Identifier,
    ReferenceManager,
};
use cairo_rs::types::errors::program_errors::ProgramError;
use cairo_rs::types::program::Program;
use cairo_rs::types::relocatable::MaybeRelocatable;
use cairo_rs::vm::errors::vm_errors::VirtualMachineError;
use cairo_rs::vm::runners::cairo_runner::CairoRunner;
use cairo_rs::vm::vm_core::VirtualMachine;
use num_bigint::{BigInt, Sign};
use num_traits::Signed;
use starknet_api::StarkFelt;

use crate::execution::entry_point::CallEntryPoint;

#[derive(Debug)]
pub enum Layout {
    All,
}

impl From<Layout> for String {
    fn from(layout: Layout) -> Self {
        // The Cairo runner is expecting `layout` to be a lowercase string.
        format!("{:?}", layout).to_ascii_lowercase()
    }
}

pub struct CairoRunConfig {
    pub enable_trace: bool,
    pub layout: Layout,
    pub proof_mode: bool,
}

impl CairoRunConfig {
    pub fn default() -> Self {
        Self { enable_trace: false, layout: Layout::All, proof_mode: false }
    }
}

pub fn felt_to_bigint(felt: StarkFelt) -> BigInt {
    BigInt::from_bytes_be(Sign::Plus, felt.bytes())
}

pub fn bigint_to_felt(bigint: &BigInt) -> Result<StarkFelt> {
    // TODO(Adi, 29/11/2022): Make sure lambdaclass always maintain that their bigints' are
    // non-negative.
    if bigint.is_negative() {
        bail!("The given BigInt, {}, is negative.", bigint)
    }

    let bigint_hex = format!("{bigint:#x}");
    match StarkFelt::from_hex(&bigint_hex) {
        Ok(felt) => Ok(felt),
        Err(e) => bail!(e),
    }
}

// TODO(Noa, 30/12/22): Remove after integrating with FN (EntryPointOffset contains usize (and not
// StarkFelt\StarkHash).
pub fn usize_try_from_starkfelt(felt: &StarkFelt) -> Result<usize> {
    let as_bytes: [u8; 8] = felt.bytes()[24..32].try_into()?;
    Ok(usize::from_be_bytes(as_bytes))
}

/// Executes a specific call to a contract entry point and returns its output.
pub fn execute_call_entry_point(
    call_entry_point: &CallEntryPoint,
    config: CairoRunConfig,
    hint_executor: &dyn HintProcessor,
) -> Result<Vec<StarkFelt>> {
    // Instantiate Cairo runner.
    let program = convert_program_to_cairo_runner_format(&call_entry_point.contract_class.program)?;
    let layout: String = config.layout.into();
    let mut cairo_runner = CairoRunner::new(&program, &layout, config.proof_mode)?;
    let mut vm = VirtualMachine::new(program.prime, config.enable_trace);
    cairo_runner.initialize_function_runner(&mut vm)?;

    // Prepare arguments for run.
    let mut args: Vec<Box<dyn Any>> = Vec::new();
    // TODO(AlonH, 21/12/2022): Push the entry point selector to args once it is used.
    // Holds pointers for all builtins which are given as implicit arguments to the entry point.
    let execution_context: Vec<MaybeRelocatable> = vm
        .get_builtin_runners()
        .iter()
        .flat_map(|(_name, builtin_runner)| builtin_runner.initial_stack())
        .collect();
    args.push(Box::new(execution_context));
    // TODO(AlonH, 21/12/2022): Consider using StarkFelt.
    // TODO(Adi, 29/11/2022): Remove the '.0' access, once derive-more is used in starknet_api.
    let calldata = &call_entry_point.calldata.0;
    args.push(Box::new(MaybeRelocatable::Int(bigint!(calldata.len()))));
    args.push(Box::new(
        calldata
            .iter()
            .map(|arg| MaybeRelocatable::Int(felt_to_bigint(*arg)))
            .collect::<Vec<MaybeRelocatable>>(),
    ));

    // Resolve initial PC from EP indicator.
    let entry_point = call_entry_point.find_entry_point_in_contract()?;
    let entry_point_pc = usize_try_from_starkfelt(&entry_point.offset.0)?;

    // Run.
    cairo_runner.run_from_entrypoint(
        entry_point_pc,
        args.iter().map(|x| x.as_ref()).collect(),
        false,
        true,
        true,
        &mut vm,
        hint_executor,
    )?;

    extract_execution_return_data(&vm)
}

fn extract_execution_return_data(vm: &VirtualMachine) -> Result<Vec<StarkFelt>> {
    let [return_data_size, return_data_ptr]: [MaybeRelocatable; 2] = vm
        .get_return_values(2)?
        .try_into()
        .unwrap_or_else(|_| panic!("Return values should be of size 2."));

    let return_data_size = match return_data_size {
        // TODO(AlonH, 21/12/2022): Handle case where res_data_size is larger than usize.
        MaybeRelocatable::Int(return_data_size) => return_data_size.bits() as usize,
        relocatable => bail!(VirtualMachineError::ExpectedInteger(relocatable)),
    };

    let values = vm.get_range(&return_data_ptr, return_data_size)?;
    // Extract values as `StarkFelt`.
    let values: Result<Vec<StarkFelt>> = values
        .into_iter()
        .map(|x| x.as_deref().cloned())
        .map(|x| match x {
            Some(MaybeRelocatable::Int(value)) => Ok(bigint_to_felt(&value)?),
            Some(relocatable) => bail!(VirtualMachineError::ExpectedInteger(relocatable)),
            None => bail!(VirtualMachineError::NoneInMemoryRange),
        })
        .collect();
    values
}

// TODO(Noa, 01/12/2022): Change this temporary solution.
pub fn convert_program_to_cairo_runner_format(
    program: &starknet_api::Program,
) -> Result<Program, ProgramError> {
    let program = program.clone();
    let identifiers = serde_json::from_value::<HashMap<String, Identifier>>(program.identifiers)?;

    let start = match identifiers.get("__main__.__start__") {
        Some(identifier) => identifier.pc,
        None => None,
    };
    let end = match identifiers.get("__main__.__end__") {
        Some(identifier) => identifier.pc,
        None => None,
    };

    Ok(Program {
        builtins: serde_json::from_value::<Vec<String>>(program.builtins)?,
        prime: deserialize_bigint_hex(program.prime)?,
        data: deserialize_array_of_bigint_hex(program.data)?,
        constants: {
            let mut constants = HashMap::new();
            for (key, value) in identifiers.iter() {
                if value.type_.as_deref() == Some("const") {
                    let value = value
                        .value
                        .clone()
                        .ok_or_else(|| ProgramError::ConstWithoutValue(key.to_owned()))?;
                    constants.insert(key.to_owned(), value);
                }
            }

            constants
        },
        main: None,
        start,
        end,
        hints: serde_json::from_value::<HashMap<usize, Vec<HintParams>>>(program.hints)?,
        reference_manager: serde_json::from_value::<ReferenceManager>(program.reference_manager)?,
        identifiers,
    })
}