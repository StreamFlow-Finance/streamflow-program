use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    msg,
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::{clock::Clock, Sysvar},
};
use std::convert::TryInto;

struct StreamFlow {
    start_time: i64,
    end_time: i64,
    amount: u64,
    withdrawn: u64,
    sender: [u8; 32],
    recipient: [u8; 32],
}

entrypoint!(process_instruction);

fn unpack_init_instruction(ix: &[u8], alice: &Pubkey, bob: &Pubkey) -> StreamFlow {
    let sf = StreamFlow {
        start_time: i64::from(u32::from_le_bytes(ix[1..5].try_into().unwrap())),
        end_time: i64::from(u32::from_le_bytes(ix[5..9].try_into().unwrap())),
        amount: u64::from_le_bytes(ix[9..17].try_into().unwrap()),
        withdrawn: 0,
        sender: alice.to_bytes(),
        recipient: bob.to_bytes(),
    };

    return sf;
}

fn unpack_account_data(ix: &[u8]) -> StreamFlow {
    let sf = StreamFlow {
        start_time: i64::from_le_bytes(ix[0..8].try_into().unwrap()),
        end_time: i64::from_le_bytes(ix[8..16].try_into().unwrap()),
        amount: u64::from_le_bytes(ix[16..24].try_into().unwrap()),
        withdrawn: u64::from_le_bytes(ix[24..32].try_into().unwrap()),
        sender: ix[32..64].try_into().unwrap(),
        recipient: ix[64..96].try_into().unwrap(),
    };

    return sf;
}

unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
    ::std::slice::from_raw_parts((p as *const T) as *const u8, ::std::mem::size_of::<T>())
}

fn initialize_stream(pid: &Pubkey, accounts: &[AccountInfo], ix: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let alice = next_account_info(account_info_iter)?;
    let bob = next_account_info(account_info_iter)?;
    let pda = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;

    if ix.len() != 17 {
        return Err(ProgramError::InvalidInstructionData);
    }

    if !pda.data_is_empty() {
        return Err(ProgramError::AccountAlreadyInitialized);
    }

    if !alice.is_writable
        || !bob.is_writable
        || !pda.is_writable
        || !alice.is_signer
        || !pda.is_signer
    {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let mut sf = unpack_init_instruction(ix, alice.key, bob.key);

    match Clock::get() {
        Ok(v) => {
            // TODO: Try on devnet
            msg!("SOLANATIME: {}", v.unix_timestamp);
            msg!("STARTTIME: {}", sf.start_time);
            msg!("ENDTIME: {}", sf.end_time);
            msg!("DURATION: {}", sf.end_time - sf.start_time);
            if sf.start_time < v.unix_timestamp || sf.start_time >= sf.end_time {
                msg!("ERROR: Timestamps are incorrect");
                return Err(ProgramError::InvalidArgument);
            }
        }
        Err(e) => return Err(e),
    }

    let struct_size = std::mem::size_of::<StreamFlow>();
    // TODO: make this rent-exempt so it's a one-time-payment
    // And on the end, return what's left to alice.
    let rent_min = Rent::default().minimum_balance(struct_size);

    // TODO: Review exact amount
    if alice.lamports() < sf.amount + rent_min {
        msg!("Not enough funds in sender's account to initialize stream");
        return Err(ProgramError::InsufficientFunds);
    }

    // Create the account holding locked funds and data
    invoke(
        &system_instruction::create_account(
            &alice.key,
            &pda.key,
            sf.amount + rent_min,
            struct_size as u64,
            &pid,
        ),
        &[alice.clone(), pda.clone(), system_program.clone()],
    )?;

    // Send enough for one transaction to Bob, so Bob can do an initial
    // withdraw without needing to have funds previously.
    // TODO: Calculate correct fees
    **pda.try_borrow_mut_lamports()? -= 5000;
    **bob.try_borrow_mut_lamports()? += 5000;
    sf.withdrawn += 5000;

    // Write our metadata to pda's data.
    let mut data = pda.try_borrow_mut_data()?;
    let bytes: &[u8] = unsafe { any_as_u8_slice(&sf) };
    data[0..bytes.len()].clone_from_slice(bytes);

    Ok(())
}

fn withdraw_unlocked(_pid: &Pubkey, accounts: &[AccountInfo], ix: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let bob = next_account_info(account_info_iter)?;
    let pda = next_account_info(account_info_iter)?;

    if ix.len() != 9 {
        return Err(ProgramError::InvalidInstructionData);
    }

    if !bob.is_signer {
        msg!("ERROR: Bob didn't sign tx");
        return Err(ProgramError::MissingRequiredSignature);
    }

    if pda.data_is_empty() {
        return Err(ProgramError::UninitializedAccount);
    }

    let data = pda.try_borrow_mut_data()?;
    let sf = unpack_account_data(&data);

    if bob.key.to_bytes() != sf.recipient {
        msg!("ERROR: bob.key != sf.recipient");
        return Err(ProgramError::InvalidArgument);
    }

    // 18:41 amountStreamed = (now - startTime)/(endTime - startTime) * amount;
    // 18:41 availableForWithdrawal = amountStreamed - lastWithdrawn;
    // 18:41 lastWithdrawn = amountStreamed;
    // -----
    // let now = 0;
    // match Clock::get() {
    // Ok(v) => now = v,
    // Err(e) => return Err(e),
    // }
    // let amount_unlocked = (now - sf.start_time) / (sf.end_time - sf.start_time) * sf.amount;
    // let available = amount_unlocked - sf.withdrawn;
    // sf.withdrawn += amount_unlocked;

    let avail = pda.lamports();
    **pda.try_borrow_mut_lamports()? -= avail;
    **bob.try_borrow_mut_lamports()? += avail;

    Ok(())
}

fn cancel_stream(_pid: &Pubkey, accounts: &[AccountInfo], _ix: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let alice = next_account_info(account_info_iter)?;
    let pda = next_account_info(account_info_iter)?;

    if !alice.is_signer {
        msg!("ERROR: Alice didn't sign tx");
        return Err(ProgramError::MissingRequiredSignature);
    }

    if pda.data_is_empty() {
        msg!("pda data is empty");
        return Err(ProgramError::UninitializedAccount);
    }

    let data = pda.try_borrow_mut_data()?;
    msg!("bytes: {:?}", &data);
    let sf = unpack_account_data(&data);

    if alice.key.to_bytes() != sf.sender {
        msg!("ERROR: alice.key != sf.sender");
        return Err(ProgramError::InvalidArgument);
    }

    // Alice decides to cancel, and withdraws from the derived account,
    // resulting in its purge.
    let avail = pda.lamports();
    **pda.try_borrow_mut_lamports()? -= avail;
    **alice.try_borrow_mut_lamports()? += avail;

    Ok(())
}

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match instruction_data[0] {
        0 => initialize_stream(program_id, accounts, instruction_data),
        1 => withdraw_unlocked(program_id, accounts, instruction_data),
        2 => cancel_stream(program_id, accounts, instruction_data),
        _ => Err(ProgramError::InvalidArgument),
    }
}
